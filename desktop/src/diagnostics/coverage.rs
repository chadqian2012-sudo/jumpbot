use super::{name, paths, Class, Report};
use crate::{
    vision::{find_dot_near, Vision},
    Config, Frame,
};
use chrono::{NaiveTime, Timelike};
use regex::Regex;
use std::path::Path;

fn seconds(text: &str, fmt: &str) -> Option<i64> {
    NaiveTime::parse_from_str(text, fmt)
        .ok()
        .map(|t| i64::from(t.num_seconds_from_midnight()))
}

pub(super) fn run(
    cfg: &Config,
    csv_path: &Path,
    dir: &Path,
    class: Class,
    limit: usize,
) -> Report<()> {
    let tag = Regex::new(r"tb=-?\d+,-?\d+,(\d+),(\d+)\s+tw=(\d+)")?;
    let center = Regex::new(r"\[center x\d+(?:\(重同步\))?\]")?;
    let mut csv = csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(csv_path)?;
    let header = csv.headers()?.clone();
    let col = |key: &str| header.iter().position(|s| s == key);
    let index = col("index");
    let time = col("timestamp");
    let score_col = col("score");
    let result = col("result");
    let note = col("note");
    let modern = col("mask_backend").is_some();
    let mut previous = None;
    let mut streak = 0i64;
    let mut selected = Vec::new();
    let gate = cfg.f("center_dot_ratio_gate");
    for record in csv.records() {
        let row = record?;
        let field = |i: Option<usize>| i.and_then(|i| row.get(i)).unwrap_or("");
        let notes = field(note);
        let landed = field(result).trim() == "landed";
        let score = field(score_col)
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|s| s as i64);
        // Historical logs infer a label from consecutive scores. New logs have an explicit
        // judgement and may span a pause, so only their recorded [center]/[miss] is authoritative.
        let label = if !landed {
            previous = None;
            streak = 0;
            continue;
        } else if modern {
            if center.is_match(notes) {
                1
            } else if notes.contains("[miss]") {
                0
            } else {
                -1
            }
        } else {
            let Some(now) = score else { continue };
            let Some(prev) = previous.replace(now) else {
                continue;
            };
            if now < prev {
                streak = 0;
                continue;
            }
            let d = now - prev;
            if d == 1 {
                streak = 0;
                0
            } else if d == ((streak + 1) * 2).min(32) {
                streak += 1;
                1
            } else if (4..=32).contains(&d) && d % 2 == 0 {
                streak = d / 2;
                1
            } else {
                -1
            }
        };
        let Some(m) = tag.captures(notes) else {
            continue;
        };
        let w = m[1].parse::<i32>()?;
        let h = m[2].parse::<i32>()?;
        let tw = m[3].parse::<f64>()?;
        let ratio = tw / f64::from(w.max(1));
        if match class {
            Class::LowRatio => ratio >= gate,
            Class::Normal => ratio < gate,
            Class::Miss => label != 0,
            Class::All => false,
        } {
            continue;
        }
        selected.push((
            field(index).to_owned(),
            seconds(field(time), "%H:%M:%S"),
            label,
            w,
            h,
            ratio,
        ));
        if limit > 0 && selected.len() == limit {
            break;
        }
    }
    let mut frames = vec![];
    for p in paths(dir, "miss_*_before.png")? {
        if let Some(t) = name(&p)
            .split('_')
            .nth(2)
            .and_then(|s| seconds(s, "%H%M%S"))
        {
            frames.push((t, p))
        }
    }
    let mut vision = Vision::new(cfg, cfg.s("masks_backend"))?;
    let mut available = 0;
    let mut shifts = Vec::new();
    for (id, t, label, w, h, ratio) in &selected {
        let Some(t) = t else { continue };
        let Some((_, p)) = frames.iter().find(|(s, _)| (s - t).abs() <= 2) else {
            continue;
        };
        let Ok(frame) = Frame::read(p) else { continue };
        available += 1;
        vision.reset_prior();
        let Some(target) = vision.detect(&frame)?.target else {
            continue;
        };
        let radius = cfg.f("center_dot_max_offset_px");
        if radius <= 0. {
            continue;
        }
        if let Some((x, y, _)) = find_dot_near(&frame, target.x, target.y, radius, cfg)? {
            shifts.push(((x - target.x).hypot(y - target.y), id, label, w, h, ratio))
        }
    }
    println!(
        "选中 {}，有现场帧 {available}，缺帧 {}，中心标记 {}/{available} ({:.1}%)",
        selected.len(),
        selected.len() - available,
        shifts.len(),
        100. * shifts.len() as f64 / available.max(1) as f64
    );
    shifts.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (d, id, label, w, h, r) in shifts.iter().take(20) {
        println!("index={id} label={label} bbox={w}x{h} ratio={r:.2} shift={d:.1}")
    }
    if !shifts.is_empty() {
        println!(
            "位移均值 {:.1}px，超过 20px 共 {} 个",
            shifts.iter().map(|s| s.0).sum::<f64>() / shifts.len() as f64,
            shifts.iter().filter(|s| s.0 > 20.).count()
        )
    }
    Ok(())
}
