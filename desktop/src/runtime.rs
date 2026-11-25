use crate::{
    asset_dir,
    masks::Masks,
    options::Options,
    screen::Screen,
    vision::{axis_components, Detection, Vision},
    Config, Frame, Result,
};
use clap::Parser;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

/// Every executable that can open an OpenCL device must recognize its own disposable child.
pub(crate) fn opencl_preflight_command() -> Option<Result<()>> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--_opencl-check") {
        return None;
    }
    let selector = args.next()?;
    if args.next().is_some() {
        return Some(Err("OpenCL 设备检查参数无效".into()));
    }
    Some(if crate::masks::opencl_probe(&selector) {
        Ok(())
    } else {
        Err("OpenCL 设备检查失败".into())
    })
}

pub fn run() -> Result<()> {
    if let Some(result) = opencl_preflight_command() {
        return result;
    }
    let o = Options::parse();
    let root = asset_dir();
    let path = o.config.clone().unwrap_or_else(|| root.join("config.json"));
    let mut cfg = if path.exists() || o.config.is_some() {
        Config::load(&path)?
    } else {
        Config::default()
    };
    o.apply(&mut cfg)?;
    if o.harvest_score {
        return crate::harvest::run(
            &o.score_dir
                .clone()
                .unwrap_or_else(|| root.join("debug/score")),
            &root.join("templates"),
        );
    }
    let screen = Screen::new(&root.join("templates"), &cfg)?;
    let mut vision = Vision::new(&cfg, cfg.s("masks_backend"))?;
    if o.selftest {
        return selftest(&cfg, &mut vision);
    }
    if !o.image.is_empty() {
        if o.bench_repeat == 0 {
            return Err("bench-repeat 必须大于零".into());
        }
        for pattern in &o.image {
            let files: Vec<PathBuf> = if Path::new(pattern).is_file() {
                vec![PathBuf::from(pattern)]
            } else {
                glob::glob(pattern)
                    .map_err(|e| e.to_string())?
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|e| e.to_string())?
            };
            if files.is_empty() {
                return Err(format!("未找到图片 {pattern}"));
            }
            for path in files {
                let capture = Frame::read(&path)?;
                let normalized = capture.resized_for_recognition(&cfg)?;
                let f = normalized.as_ref().unwrap_or(&capture);
                vision.reset_prior();
                let mut d = vision.detect(f)?;
                let mut times = Vec::new();
                for _ in 0..o.bench_repeat {
                    vision.reset_prior();
                    let t = Instant::now();
                    d = vision.detect(f)?;
                    times.push(t.elapsed().as_secs_f64() * 1e6);
                }
                let mut row = detection(&d, &cfg);
                row["calibrated_dist_px"] = row["dist_px"].clone();
                if normalized.is_some() {
                    map_detection_to_capture(
                        &mut d,
                        capture.width as f64 / f.width as f64,
                        capture.height as f64 / f.height as f64,
                    );
                    row["piece"] = json!(d.piece);
                    row["target"] = json!(d.target);
                    row["dist_px"] = json!(distance_px(&d, &cfg));
                }
                row["capture_size"] = json!([capture.width, capture.height]);
                row["recognition_size"] = json!([f.width, f.height]);
                row["image"] = json!(path);
                row["backend"] = json!(vision.backend());
                row["score"] = json!(screen.score(f, &cfg));
                row["piece_mask_crc32"] = json!(crc32fast::hash(&d.piece_mask));
                row["block_mask_crc32"] = json!(crc32fast::hash(&d.block_mask));
                row["detect_us"] = json!(times);
                println!("{row}");
                if let Some(dir) = &o.out_dir {
                    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                    annotated(&capture, &d).write(&dir.join(format!(
                        "{}_annotated.png",
                        path.file_stem().unwrap_or_default().to_string_lossy()
                    )))?;
                }
            }
        }
        return Ok(());
    }
    crate::play::run(&o, &cfg, &screen, &mut vision, &path)
}
// The public image result and its annotation use the caller's pixel coordinates.
fn map_detection_to_capture(d: &mut Detection, sx: f64, sy: f64) {
    let area = |n: i32| (f64::from(n) * sx * sy).round() as i32;
    let bbox = |[x, y, w, h]: [i32; 4]| {
        let left = (f64::from(x) * sx).round() as i32;
        let top = (f64::from(y) * sy).round() as i32;
        [
            left,
            top,
            (f64::from(x + w) * sx).round() as i32 - left,
            (f64::from(y + h) * sy).round() as i32 - top,
        ]
    };
    if let Some(p) = &mut d.piece {
        p.x *= sx;
        p.y *= sy;
        p.bbox = bbox(p.bbox);
        p.area = area(p.area);
    }
    if let Some(t) = &mut d.target {
        t.x *= sx;
        t.y *= sy;
        t.bbox = bbox(t.bbox);
        t.area = area(t.area);
        t.top_y *= sy;
        t.top_width *= sx;
        t.dot = t.dot.map(|(x, y, n)| (x * sx, y * sy, area(n)));
    }
}

fn distance_px(d: &Detection, c: &Config) -> Option<f64> {
    d.piece
        .as_ref()
        .zip(d.target.as_ref())
        .map(|(p, t)| axis_components(t.x - p.x, t.y - p.y, c.f("axis_slope")).1)
}
pub(crate) fn detection(d: &Detection, c: &Config) -> Value {
    let dist = distance_px(d, c);
    json!({"piece":d.piece,"target":d.target,"dist_px":dist,"hold_ms":dist.map(|v|v*c.f("k")+c.f("b"))})
}
pub(crate) fn annotated(f: &Frame, d: &Detection) -> Frame {
    let mut image = f.clone();
    let mut mark = |x: i32, y: i32, color: [u8; 3]| {
        if x >= 0 && y >= 0 && (x as usize) < f.width && (y as usize) < f.height {
            let i = (y as usize * f.width + x as usize) * 3;
            image.bgr[i..i + 3].copy_from_slice(&color)
        }
    };
    let items = [
        d.piece.as_ref().map(|p| (p.x, p.y, p.bbox, [0, 0, 255])),
        d.target.as_ref().map(|p| (p.x, p.y, p.bbox, [0, 255, 0])),
    ];
    for (x, y, [l, t, w, h], color) in items.into_iter().flatten() {
        for px in l..l + w {
            mark(px, t, color);
            mark(px, t + h - 1, color)
        }
        for py in t..t + h {
            mark(l, py, color);
            mark(l + w - 1, py, color)
        }
        for i in -8..=8 {
            mark(x as i32 + i, y as i32, color);
            mark(x as i32, y as i32 + i, color)
        }
    }
    image
}
fn selftest(cfg: &Config, vision: &mut Vision) -> Result<()> {
    let f = Frame {
        width: 553,
        height: 1050,
        bgr: (0..553 * 1050 * 3)
            .map(|i| ((i / 553 + i % 251) % 256) as u8)
            .collect(),
    };
    Frame::read(&asset_dir().join("templates/piece.png"))?;
    let reference = Masks::new(cfg, "cpu").compute(&f)?;
    for name in ["cpu", "simd", "cuda", "opencl", "auto"] {
        let mut masks = Masks::new(cfg, name);
        let start = Instant::now();
        let pair = masks.compute(&f)?;
        if reference != pair {
            return Err(format!("{name} 掩码与 CPU 结果不同"));
        }
        println!(
            "{}",
            json!({"requested":name,"actual":masks.backend(),"mask_equal":true,"ms":start.elapsed().as_secs_f64()*1000.})
        );
    }
    vision.detect(&f)?;
    println!("原生自检通过");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_board_keeps_its_hold_after_window_scaling() {
        let cfg = Config::default();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/fixtures");
        let mut vision = Vision::new(&cfg, "cpu").unwrap();
        for (name, width, height) in [
            ("skin-gold-cardboard.png", 830, 1575),
            ("purple-cup-before-death.png", 415, 788),
            ("purple-horizontal-fall.png", 720, 900),
        ] {
            let reference = Frame::read(&root.join(name)).unwrap();
            vision.reset_prior();
            let expected = detection(&vision.detect(&reference).unwrap(), &cfg);
            // Synthesize a different client resolution without the production resizer.
            let mut pixels = Vec::with_capacity(width * height * 3);
            for y in 0..height {
                for x in 0..width {
                    let offset = 3
                        * ((y * reference.height / height) * reference.width
                            + x * reference.width / width);
                    pixels.extend_from_slice(&reference.bgr[offset..offset + 3]);
                }
            }
            let capture = Frame {
                width,
                height,
                bgr: pixels,
            };
            let normalized = capture.for_recognition(&cfg).unwrap();
            assert_eq!((normalized.width, normalized.height), (553, 1050));
            vision.reset_prior();
            let mut d = vision.detect(&normalized).unwrap();
            let actual = detection(&d, &cfg);
            if let Some(ms) = expected["hold_ms"].as_f64() {
                assert!(
                    (actual["hold_ms"].as_f64().unwrap() - ms).abs() < 8.0,
                    "{name}: {actual}"
                );
                map_detection_to_capture(&mut d, width as f64 / 553., height as f64 / 1050.);
                let target = d.target.unwrap();
                assert!(
                    (target.x * 553. / width as f64 - expected["target"]["x"].as_f64().unwrap())
                        .abs()
                        < 3.0
                );
            } else {
                assert!(actual["hold_ms"].is_null(), "{name}: {actual}");
            }
        }
    }

    #[test]
    fn release_frames_match_center_decisions() {
        let cfg = Config::default();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let screen = Screen::new(&root.join("templates"), &cfg).unwrap();
        for backend in ["cpu", "simd"] {
            let mut vision = Vision::new(&cfg, backend).unwrap();
            for line in include_str!("../../tests/fixtures/v1.3-decisions.jsonl").lines() {
                let original: Value = serde_json::from_str(line).unwrap();
                let name = original["file"].as_str().unwrap();
                let mut target = original["target"].clone();
                // Midpoints and pixel counts of the marked regions in the retained PNGs.
                let mark = match name {
                    "skin-gold-cardboard-seam.png" => Some((231.5, 532.5, 183)), // (222,527)..=(241,538)
                    "skin-gold-white-book.png" => Some((421.0, 489.0, 157)), // (412,484)..=(430,494)
                    _ => None,
                };
                if let Some((x, y, area)) = mark {
                    target["x"] = json!(x);
                    target["y"] = json!(y);
                    target["dot"] = json!([x, y, area]);
                }
                let frame = Frame::read(&root.join("tests/fixtures").join(name)).unwrap();
                vision.reset_prior();
                let d = vision.detect(&frame).unwrap();
                let actual = detection(&d, &cfg);
                for (kind, label) in [("piece", &original["piece"]), ("target", &target)] {
                    assert_eq!(
                        actual[kind].is_null(),
                        label.is_null(),
                        "{name} {kind} {backend}"
                    );
                    if let Some(fields) = label.as_object() {
                        for (field, v) in fields {
                            assert_eq!(&actual[kind][field], v, "{name} {kind}.{field} {backend}");
                        }
                    }
                }
                assert_eq!(
                    json!(screen.score(&frame, &cfg)),
                    original["score"],
                    "{name} HUD"
                );
                assert_eq!(
                    json!(crc32fast::hash(&d.piece_mask)),
                    original["piece_mask"],
                    "{name} piece mask"
                );
                assert_eq!(
                    json!(crc32fast::hash(&d.block_mask)),
                    original["block_mask"],
                    "{name} block mask"
                );
            }
        }
    }
}
