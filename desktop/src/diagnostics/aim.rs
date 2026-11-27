use super::{median, name, paths, Report};
use crate::{
    vision::{axis_components, find_dot_near, Vision},
    Config, Frame,
};
use std::{fs, path::Path};

const HEADER: [&str; 26] = [
    "src",
    "file",
    "bbox_x",
    "bbox_y",
    "bbox_w",
    "bbox_h",
    "top_y",
    "top_width",
    "ratio",
    "area",
    "gate",
    "plain_x",
    "plain_y",
    "truth_x",
    "truth_y",
    "truth_area",
    "dot_dist",
    "err_along",
    "err_cross",
    "piece_x",
    "piece_y",
    "along_dist",
    "cross_ratio",
    "relaxed_ok",
    "relaxed_dx",
    "relaxed_dy",
];

fn annotated(frame: &Frame) -> bool {
    let (mut green, mut red) = (0, 0);
    for &[b, g, r] in frame.bgr.as_chunks::<3>().0 {
        green += usize::from(g > 200 && r < 80 && b < 80);
        red += usize::from(r > 200 && g < 80 && b < 80);
        if green > 200 || red > 200 {
            return true;
        }
    }
    false
}
fn section(label: &str, v: &mut [f64]) {
    if v.is_empty() {
        println!("{label} 无样本");
        return;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let abs: Vec<_> = v.iter().map(|v| v.abs()).collect();
    println!(
        "{label} n={n} mean={:.1} median={:.1} abs_median={:.1} p90={:.1} max={:.1}",
        v.iter().sum::<f64>() / n as f64,
        median(v),
        median(&abs),
        v[n * 9 / 10],
        v[n - 1]
    );
}
pub(super) fn run(cfg: &Config, debug: &Path, roots: &str, max: usize, out: &Path) -> Report<()> {
    let radius = cfg.f("center_dot_max_offset_px");
    if radius <= 0. {
        return Err("center_dot_max_offset_px 必须大于 0".into());
    }
    let mut files = Vec::new();
    for pat in roots.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        for entry in paths(debug, pat)? {
            if entry.is_dir() {
                let run = name(&entry).starts_with("run_");
                files.extend(paths(&entry, "*.png")?.into_iter().filter(|p| {
                    let f = name(p);
                    !run || !(f.ends_with("_before.png") || f.ends_with("_after.png"))
                }));
            } else if entry
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
            {
                files.push(entry)
            }
        }
    }
    if max > 0 {
        files.truncate(max)
    }
    let mut dotted = Vision::new(cfg, cfg.s("masks_backend"))?;
    let mut base = cfg.clone();
    base.0["center_dot_max_offset_px"] = 0.into();
    let mut plain = Vision::new(&base, base.s("masks_backend"))?;
    if let Some(p) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p)?
    }
    let mut writer = csv::Writer::from_path(out)?;
    writer.write_record(HEADER)?;
    let (mut skipped, mut count) = (0, 0);
    let mut errs: [Vec<f64>; 3] = Default::default();
    let mut bands = [[0usize; 3]; 4];
    let mut extra = vec![];
    for path in &files {
        let Ok(f) = Frame::read(path) else { continue };
        if annotated(&f) {
            skipped += 1;
            continue;
        }
        plain.reset_prior();
        let d = plain.detect(&f)?;
        let (Some(p), Some(t)) = (d.piece, d.target) else {
            continue;
        };
        dotted.reset_prior();
        let truth = dotted.detect(&f)?.target.and_then(|t| t.dot);
        let rel = find_dot_near(&f, t.x, t.y, radius, cfg)?;
        let [x, y, w, h] = t.bbox;
        let ratio = t.top_width / f64::from(w.max(1));
        let gate = ratio < cfg.f("center_dot_ratio_gate")
            && f64::from(w) >= cfg.f("center_dot_min_block_width");
        let pos = truth.map(|(u, v, _)| axis_components(u - p.x, v - p.y, cfg.f("axis_slope")));
        let error = truth.map(|(u, v, _)| axis_components(t.x - u, t.y - v, cfg.f("axis_slope")));
        let dist = truth.map(|(u, v, _)| (t.x - u).hypot(t.y - v));
        let round = |v: f64| (v * 100.).round_ties_even() / 100.;
        if let (Some((_, a, b)), Some(dist)) = (error, dist) {
            for (vec, val) in errs.iter_mut().zip([a, b, dist]) {
                vec.push(round(val))
            }
        }
        let band = match w {
            0..80 => Some(0),
            80..120 => Some(1),
            120..200 => Some(2),
            200..4000 => Some(3),
            _ => None,
        };
        if let Some(i) = band {
            bands[i][0] += 1;
            bands[i][1] += usize::from(truth.is_some());
            bands[i][2] += usize::from(rel.is_some())
        }
        let delta = rel.map(|(u, v, _)| (u - t.x, v - t.y));
        if let Some((dx, dy)) = delta.filter(|_| w < 120 && truth.is_none()) {
            extra.push((round(dx).hypot(round(dy)), name(path)))
        }
        let num = |v: Option<f64>| v.map_or_else(String::new, |x| format!("{x:.2}"));
        writer.write_record([
            name(path.parent().unwrap()).into(),
            name(path).into(),
            x.to_string(),
            y.to_string(),
            w.to_string(),
            h.to_string(),
            format!("{:.2}", t.top_y),
            format!("{:.2}", t.top_width),
            format!("{ratio:.4}"),
            t.area.to_string(),
            u8::from(gate).to_string(),
            format!("{:.2}", t.x),
            format!("{:.2}", t.y),
            num(truth.map(|b| b.0)),
            num(truth.map(|b| b.1)),
            truth.map_or_else(String::new, |b| b.2.to_string()),
            num(dist),
            num(error.map(|u| u.1)),
            num(error.map(|u| u.2)),
            format!("{:.2}", p.x),
            format!("{:.2}", p.y),
            num(pos.map(|u| u.1)),
            pos.filter(|u| u.1 != 0.)
                .map_or_else(String::new, |u| format!("{:.4}", u.2 / u.1.abs().max(1.))),
            u8::from(rel.is_some()).to_string(),
            num(delta.map(|d| d.0)),
            num(delta.map(|d| d.1)),
        ])?;
        count += 1;
    }
    writer.flush()?;
    println!(
        "候选 {}，跳过标注 {skipped}，有目标 {count}，其中白点 {} ({:.1}%)，CSV {}",
        files.len(),
        errs[0].len(),
        100. * errs[0].len() as f64 / count.max(1) as f64,
        out.display()
    );
    for (label, vec) in ["沿轴误差", "横向误差", "欧氏距离"]
        .into_iter()
        .zip(&mut errs)
    {
        section(label, vec)
    }
    for (band, [n, t, r]) in ["0~80", "80~120", "120~200", "200~4000"]
        .into_iter()
        .zip(bands)
    {
        if n > 0 {
            println!(
                "宽度 {band} n={n} 命中 {:.1}% 放宽 {:.1}%",
                100. * t as f64 / n as f64,
                100. * r as f64 / n as f64
            )
        }
    }
    println!("窄方块新增候选 {}", extra.len());
    if !extra.is_empty() {
        extra.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut d: Vec<_> = extra.iter().map(|v| v.0).collect();
        section("新增白点偏移", &mut d);
        for (dx, file) in extra.iter().take(5) {
            println!("  {file} {dx:.1}px")
        }
    }
    Ok(())
}
