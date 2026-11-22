use super::{median, name, paths, Report};
use crate::{
    landing,
    vision::{axis_components, find_dot_near, Vision},
    Config, Frame,
};
use std::path::{Path, PathBuf};

pub(super) fn paired(dir: &Path, raw: bool) -> Report<Vec<(PathBuf, PathBuf)>> {
    let mut pairs = Vec::new();
    for before in paths(
        dir,
        if raw {
            "jump_*_raw.png"
        } else {
            "*_before.png"
        },
    )? {
        let f = name(&before);
        let after = if raw {
            let Some(digits) = f
                .strip_prefix("jump_")
                .and_then(|s| s.strip_suffix("_raw.png"))
                .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            else {
                continue;
            };
            before.with_file_name(format!("jump_{digits}_raw_after.png"))
        } else {
            before.with_file_name(format!(
                "{}_after.png",
                f.strip_suffix("_before.png").unwrap()
            ))
        };
        if after.is_file() {
            pairs.push((before, after))
        }
    }
    Ok(pairs)
}

pub(super) fn measure(cfg: &Config, dir: &Path, tol: f64, truth: Option<bool>) -> Report<bool> {
    let pairs = paired(dir, false)?;
    if pairs.is_empty() && truth.is_none() {
        return Err(format!("{} 无 *_before/_after 配对", dir.display()).into());
    }
    let mut mask_cfg = cfg.clone();
    if mask_cfg.s("landing_method") == "flow" {
        mask_cfg.0["landing_method"] = "auto".into()
    }
    let mut flow_cfg = cfg.clone();
    flow_cfg.0["landing_method"] = "flow".into();
    let mut vision = Vision::new(cfg, cfg.s("masks_backend"))?;
    let (mut valid, mut mask_available, mut flow_available, mut mask_hits, mut flow_hits) =
        (0, 0, 0, 0, 0);
    let (mut estimates, mut observed) = (vec![], vec![]);
    println!("配对 {} 组", pairs.len());
    for (before, after) in pairs {
        let (Ok(f), Ok(g)) = (Frame::read(&before), Frame::read(&after)) else {
            println!("{} 读图失败", name(&before));
            continue;
        };
        vision.reset_prior();
        let d = vision.detect(&f)?;
        let (Some(p), Some(t)) = (d.piece, d.target) else {
            println!("{} 起跳帧识别失败", name(&before));
            continue;
        };
        let end = vision.detect(&g)?;
        let Some(land) = &end.piece else {
            println!("{} 落地帧无棋子", name(&before));
            continue;
        };
        let (axis, _, _) = axis_components(t.x - p.x, t.y - p.y, cfg.f("axis_slope"));
        let mask = landing::error(&f, &g, &end, &t, &mask_cfg, axis).map(|a| a.0);
        valid += 1;
        mask_available += usize::from(mask.is_some());
        if let Some(list) = truth {
            let Some(est) = mask else { continue };
            let anchor = land.y + cfg.f("piece_anchor_dy_ratio") * g.height as f64;
            let center = (land.x - est * axis.0, anchor + 18.);
            if let Some((x, y, _)) = find_dot_near(&g, center.0, center.1, 70., cfg)? {
                let err = (land.x - x) * axis.0 + (anchor - y) * axis.1;
                observed.push(err);
                estimates.push(est);
                println!(
                    "{} estimate={est:.1} true={err:.1} dot_offset={:.1}",
                    name(&before),
                    (center.0 - x).hypot(center.1 - y)
                )
            } else if list {
                println!("{} estimate={est:.1} 白点缺失", name(&before))
            }
        } else {
            let flow = landing::error(&f, &g, &end, &t, &flow_cfg, axis).map(|a| a.0);
            flow_available += usize::from(flow.is_some());
            mask_hits += usize::from(mask.is_some_and(|v| v.abs() < tol));
            flow_hits += usize::from(flow.is_some_and(|v| v.abs() < tol));
            println!(
                "{} mask={:.1} phase={:.1}",
                name(&before),
                mask.unwrap_or(f64::NAN),
                flow.unwrap_or(f64::NAN)
            );
        }
    }
    let percent = |n: usize, d: usize| 100. * n as f64 / d.max(1) as f64;
    if truth.is_some() {
        let n = observed.len();
        println!("有估计误差 {mask_available} 帧，其中白点 {n}");
        if n > 0 {
            let abs: Vec<_> = observed.iter().map(|x| x.abs()).collect();
            println!(
                "估计 mean={:.2} median={:.2} 真值 mean={:.2} median={:.2} abs_median={:.1}",
                estimates.iter().sum::<f64>() / n as f64,
                median(&estimates),
                observed.iter().sum::<f64>() / n as f64,
                median(&observed),
                median(&abs)
            );
            let over = abs.iter().filter(|v| **v > tol).count();
            println!("真实偏移超过 {tol}px {over}/{n} ({:.1}%)", percent(over, n));
        }
    } else {
        println!("真实未命中 {valid} 组内误判命中（阈值 {tol}px） 掩码 {mask_hits}/{valid} ({:.1}%) 相位 {flow_hits}/{valid} ({:.1}%)；相位可用 {flow_available}/{valid}",percent(mask_hits,valid),percent(flow_hits,valid))
    }
    Ok(true)
}
pub(super) fn replay(cfg: &Config, debug: &Path, dir: Option<&Path>, limit: usize) -> Report<bool> {
    use crate::screen::Screen;
    use std::time::Instant;
    let mut samples = Vec::new();
    if let Some(d) = dir {
        samples = paired(d, true)?
    } else {
        for d in paths(debug, "run_*")? {
            let p = paired(&d, true)?;
            if p.len() > samples.len() {
                samples = p
            }
        }
    }
    samples.truncate(limit);
    if samples.is_empty() {
        return Err("没有完整的 jump_NNN_raw.png / jump_NNN_raw_after.png 原图对".into());
    }
    let mut vision = Vision::new(cfg, cfg.s("masks_backend"))?;
    let screen = Screen::new(&crate::asset_dir().join("templates"), cfg)?;
    let load = |p: &Path| Frame::read(p)?.for_recognition(cfg);
    let (mut stable_count, mut acquired_count) = (0, 0);
    let (mut land_ms, mut acquire_ms) = (Vec::new(), Vec::new());
    println!(
        "重放目录 {}，{} 跳",
        samples[0].0.parent().unwrap().display(),
        samples.len()
    );
    for (raw, after) in &samples {
        let (Ok(a), Ok(b)) = (load(raw), load(after)) else {
            println!("{} 帧读不到", name(raw));
            continue;
        };
        vision.reset_prior();
        let t = Instant::now();
        let (start, end) = (a.motion_sample(cfg)?, b.motion_sample(cfg)?);
        let movement = start
            .iter()
            .zip(&end)
            .map(|(x, y)| u64::from(x.abs_diff(*y)))
            .sum::<u64>() as f64
            / start.len() as f64;
        let mut previous: Option<(f64, f64)> = None;
        let mut stable = 0i64;
        let needed = cfg.i("settle_frames").max(1);
        let budget =
            (cfg.f("settle_timeout_s") * 1000. / cfg.f("settle_poll_ms").max(1.)).ceil() as i64;
        // The raw pair records the endpoints only. The first after sample is compared
        // with takeoff; every subsequent after sample repeats the same recorded frame.
        if !screen.state(&b, cfg)?.is_result() {
            for poll in 1..needed.saturating_add(4).min(budget) {
                let diff = if poll == 1 { movement } else { 0. };
                if diff >= cfg.f("settle_diff_threshold") {
                    continue;
                }
                let d = vision.detect_piece_only(&b)?;
                let observed = d.piece.as_ref().map(|p| (p.x, p.y));
                let close = previous.zip(observed).is_some_and(|((x, y), (u, v))| {
                    (x - u).abs() <= cfg.f("settle_anchor_tol_px")
                        && (y - v).abs() <= cfg.f("settle_anchor_tol_px")
                });
                stable = if close && landing::foot(&b, &d) {
                    stable + 1
                } else {
                    0
                };
                previous = observed;
                if stable >= needed {
                    break;
                }
            }
        }
        let landed = stable >= needed;
        stable_count += usize::from(landed);
        land_ms.push(t.elapsed().as_secs_f64() * 1000.);
        vision.reset_prior();
        let t = Instant::now();
        let mut last = None;
        let mut acquired = false;
        let state = screen.state(&a, cfg)?;
        if matches!(
            state,
            crate::screen::ScreenState::Board | crate::screen::ScreenState::UnknownDark
        ) {
            for _ in 0..cfg.i("detect_retry_limit").clamp(1, 3) {
                let d = vision.detect(&a)?;
                if !state.playable(&a, cfg, &screen, &d) {
                    break;
                }
                let current = d
                    .piece
                    .zip(d.target)
                    .map(|(_, target)| (target.x, target.y));
                if last
                    .zip(current)
                    .is_some_and(|((x, y), (u, v)): ((f64, f64), (f64, f64))| {
                        (x - u).abs() <= cfg.f("target_consistency_px")
                            && (y - v).abs() <= cfg.f("target_consistency_px")
                    })
                {
                    acquired = true;
                    break;
                }
                last = current;
            }
        }
        acquired_count += usize::from(acquired);
        acquire_ms.push(t.elapsed().as_secs_f64() * 1000.);
        println!(
            "{} landed={landed} acquire={acquired} motion={movement:.2}",
            name(raw)
        );
    }
    let n = samples.len();
    println!("落地 {stable_count}/{n} 起跳前确认 {acquired_count}/{n}");
    if !land_ms.is_empty() {
        println!(
            "离线计算中位数（不含等待）落地 {:.0}ms 起跳确认 {:.0}ms",
            median(&land_ms),
            median(&acquire_ms)
        )
    }
    Ok(stable_count == n && acquired_count == n)
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay_uses_only_complete_numbered_raw_pairs() {
        let d = tempfile::tempdir().unwrap();
        for s in [
            "001_raw",
            "001_raw_after",
            "002_before",
            "002_after",
            "002_raw",
            "foo_raw",
            "foo_raw_after",
        ] {
            std::fs::write(d.path().join(format!("jump_{s}.png")), []).unwrap();
        }
        assert_eq!(
            super::paired(d.path(), true).unwrap(),
            [(
                d.path().join("jump_001_raw.png"),
                d.path().join("jump_001_raw_after.png")
            )]
        );
    }
}
