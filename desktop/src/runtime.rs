use crate::{
    asset_dir,
    masks::Masks,
    options::Options,
    screen::Screen,
    vision::{axis_components, Block, Detection, Vision},
    Config, Frame, Result,
};
use clap::Parser;
use serde_json::{json, Value};
use std::{
    fs,
    num::NonZeroUsize,
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
        let timing = JumpTiming::initial(&cfg, &o);
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
                let (normalized, rows) = capture.resized_for_recognition(&cfg)?;
                let f = normalized.as_ref().unwrap_or(&capture);
                let (mut d, times) = measure_image(o.bench_repeat, || {
                    vision.reset_prior();
                    let t = Instant::now();
                    let d = vision.detect(f)?;
                    Ok((d, t.elapsed().as_secs_f64() * 1e6))
                })?;
                let mut row = timing.detection(&d, None);
                row["calibrated_dist_px"] = row["dist_px"].clone();
                if normalized.is_some() {
                    map_detection_to_capture(&mut d, capture.width as f64 / f.width as f64, rows);
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

// An ordinary image keeps its first sample. An explicit benchmark discards it as warm-up.
fn measure_image<T>(
    repeat: Option<NonZeroUsize>,
    mut detect: impl FnMut() -> Result<(T, f64)>,
) -> Result<(T, Vec<f64>)> {
    let (mut detection, first_us) = detect()?;
    let Some(repeat) = repeat else {
        return Ok((detection, vec![first_us]));
    };
    let mut times = Vec::new();
    for _ in 0..repeat.get() {
        let (next, us) = detect()?;
        detection = next;
        times.push(us);
    }
    Ok((detection, times))
}

// The public image result and its annotation use the caller's pixel coordinates.
pub(crate) fn map_detection_to_capture(
    d: &mut Detection,
    sx: f64,
    rows: crate::screen::CaptureRows,
) {
    let sy = rows.board_scale();
    let area = |n: i32| (f64::from(n) * sx * sy).round() as i32;
    let bbox = |[x, y, w, h]: [i32; 4]| {
        let left = (f64::from(x) * sx).round() as i32;
        let top = rows.capture_y(f64::from(y)).round() as i32;
        [
            left,
            top,
            (f64::from(x + w) * sx).round() as i32 - left,
            rows.capture_y(f64::from(y + h)).round() as i32 - top,
        ]
    };
    if let Some(p) = &mut d.piece {
        p.x *= sx;
        p.y = rows.capture_y(p.y);
        p.bbox = bbox(p.bbox);
        p.area = area(p.area);
    }
    if let Some(t) = &mut d.target {
        t.x *= sx;
        t.y = rows.capture_y(t.y);
        t.bbox = bbox(t.bbox);
        t.area = area(t.area);
        t.top_y = rows.capture_y(t.top_y);
        t.top_width *= sx;
        t.dot = t.dot.map(|(x, y, n)| (x * sx, rows.capture_y(y), area(n)));
    }
}

fn distance_px(d: &Detection, c: &Config) -> Option<f64> {
    d.piece
        .as_ref()
        .zip(d.target.as_ref())
        .map(|(p, t)| axis_components(t.x - p.x, t.y - p.y, c.f("axis_slope")).1)
}

pub(crate) struct JumpTiming<'a> {
    cfg: &'a Config,
    calibrate: bool,
    pub k: f64,
    pub bias_pct: f64,
}

impl<'a> JumpTiming<'a> {
    pub(crate) fn initial(cfg: &'a Config, o: &Options) -> Self {
        Self::new(cfg, o, cfg.f("k"), 0, 0)
    }

    pub(crate) fn new(cfg: &'a Config, o: &Options, k: f64, band: usize, jump_index: u64) -> Self {
        Self {
            cfg,
            calibrate: o.calibrate,
            k: if o.calibrate {
                cfg.f("k") * o.calib_k_scales[band]
            } else {
                k
            },
            bias_pct: if o.bias_sweep {
                o.bias_levels[jump_index as usize / o.bias_per_level.max(1) % o.bias_levels.len()]
            } else {
                0.
            },
        }
    }

    fn clamp_ms(&self, ms: f64) -> f64 {
        ms.clamp(
            self.cfg.f("press_duration_min_ms"),
            self.cfg.f("press_duration_max_ms"),
        )
    }

    pub(crate) fn distance_and_hold(&self, t: &Block, mut distance: f64, ema: f64) -> (f64, f64) {
        let cfg = self.cfg;
        if cfg.f("low_ratio_trim_ratio") > 0.
            && t.top_width < cfg.f("low_ratio_gate") * f64::from(t.bbox[2])
        {
            distance *= 1. - cfg.f("low_ratio_trim_ratio");
        }
        distance *= 1. + self.bias_pct / 100.;
        let mut hold = if !self.calibrate && cfg.f("transfer_a") > 0.01 {
            (distance - cfg.f("transfer_c")) / cfg.f("transfer_a")
        } else {
            self.k * distance + cfg.f("b")
        };
        hold *= 1. + cfg.f("press_bias_ratio");
        if !self.calibrate && cfg.f("gamma") > 0. {
            let trim = cfg.f("residual_trim_ratio");
            hold *= 1. + (-0.3 * ema).clamp(-trim, trim);
        }
        (distance, self.clamp_ms(hold))
    }

    pub(crate) fn detection(&self, d: &Detection, explicit_ms: Option<f64>) -> Value {
        let dist = distance_px(d, self.cfg);
        let hold = dist
            .zip(d.target.as_ref())
            .map(|(distance, target)| match explicit_ms {
                Some(ms) => self.clamp_ms(ms),
                None => self.distance_and_hold(target, distance, 0.).1,
            });
        json!({"piece":d.piece,"target":d.target,"dist_px":dist,"hold_ms":hold})
    }
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
    fn image_benchmarks_discard_only_the_explicit_warmup() {
        for (args, samples) in [
            (vec!["jumpbot"], vec![1.]),
            (vec!["jumpbot", "--bench-repeat", "1"], vec![2.]),
            (vec!["jumpbot", "--bench-repeat", "3"], vec![2., 3., 4.]),
        ] {
            let repeat = Options::parse_from(args).bench_repeat;
            let mut calls = 0;
            let (last, times) = measure_image(repeat, || {
                calls += 1;
                Ok((calls, f64::from(calls)))
            })
            .unwrap();
            assert_eq!(times, samples);
            assert_eq!(f64::from(calls), *samples.last().unwrap());
            assert_eq!(last, calls);
        }
        assert!(Options::try_parse_from(["jumpbot", "--bench-repeat", "0"]).is_err());
    }

    #[test]
    fn live_uses_adjusted_distance_and_image_reports_geometry() {
        let mut cfg = Config::default();
        let mut o = Options::parse_from([
            "jumpbot",
            "--bias-sweep",
            "--bias-levels=25,0",
            "--bias-per-level=1",
            "--calib-k-scales=0.5,2",
            "--k=2",
            "--gamma=1",
        ]);
        o.apply(&mut cfg).unwrap();
        cfg.0["low_ratio_trim_ratio"] = json!(0.25);
        cfg.0["press_bias_ratio"] = json!(0.25);
        cfg.0["transfer_a"] = json!(0.5);
        cfg.0["transfer_c"] = json!(7.5);
        cfg.0["residual_trim_ratio"] = json!(0.125);
        cfg.0["b"] = json!(4.);
        cfg.0["axis_slope"] = json!(0.);
        let d = Detection {
            piece: Some(crate::vision::Piece {
                x: 100.,
                y: 500.,
                bbox: [80, 460, 40, 40],
                area: 800,
                tpl_score: 1.,
                source: "legacy",
                shape_score: 1.,
            }),
            target: Some(Block {
                x: 300.,
                y: 500.,
                bbox: [250, 490, 100, 40],
                area: 2000,
                top_y: 490.,
                top_width: 20.,
                dot: None,
            }),
            unresolved_target_shadow: false,
            piece_mask: vec![],
            block_mask: vec![],
        };
        let target = d.target.as_ref().unwrap();
        let timing = JumpTiming::initial(&cfg, &o);
        let row = timing.detection(&d, None);
        assert_eq!(row["dist_px"], 200.);
        // 200 * 0.75 * 1.25 = 187.5 timing pixels. Transfer and press bias give 450 ms.
        assert_eq!(row["hold_ms"], 450.);
        assert_eq!(timing.distance_and_hold(target, 200., 0.), (187.5, 450.));
        assert_eq!(timing.distance_and_hold(target, 200., 1.), (187.5, 393.75));
        assert_eq!(timing.distance_and_hold(target, 0., 0.).1, 60.);
        assert_eq!(timing.distance_and_hold(target, 10000., 0.).1, 1500.);
        for (manual, expected) in [(5., 60.), (123., 123.), (3000., 1500.)] {
            assert_eq!(timing.detection(&d, Some(manual))["hold_ms"], expected);
        }

        o.no_adaptive = true;
        o.apply(&mut cfg).unwrap();
        let fixed = JumpTiming::initial(&cfg, &o);
        assert_eq!(fixed.distance_and_hold(target, 200., 1.), (187.5, 450.));
        o.no_adaptive = false;
        o.calibrate = true;
        o.apply(&mut cfg).unwrap();
        assert_eq!(
            JumpTiming::initial(&cfg, &o).detection(&d, None)["hold_ms"],
            239.375
        );
        let second = JumpTiming::new(&cfg, &o, 1., 1, 1);
        // Second calibration band uses k=4 and zero sweep bias, bypassing transfer and residual.
        assert_eq!(second.distance_and_hold(target, 200., 1.), (150., 755.));
    }

    #[test]
    fn same_board_keeps_its_hold_after_window_scaling() {
        let cfg = Config::default();
        let timing = JumpTiming::initial(&cfg, &Options::parse_from(["jumpbot"]));
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/fixtures");
        let mut vision = Vision::new(&cfg, "cpu").unwrap();
        for (name, width, height, fixed_bar) in [
            ("skin-gold-cardboard.png", 830, 1575, 0),
            ("purple-cup-before-death.png", 415, 788, 0),
            ("purple-horizontal-fall.png", 720, 900, 0),
            ("purple-cup-before-death.png", 415, 788, 66),
            ("skin-gold-cardboard.png", 830, 1575, 66),
        ] {
            let reference = Frame::read(&root.join(name)).unwrap();
            vision.reset_prior();
            let expected = timing.detection(&vision.detect(&reference).unwrap(), None);
            // Synthesize a different client resolution without the production resizer.
            let mut pixels = Vec::with_capacity(width * height * 3);
            for y in 0..height {
                let source_y = if y < fixed_bar {
                    y
                } else {
                    fixed_bar
                        + (y - fixed_bar) * (reference.height - fixed_bar) / (height - fixed_bar)
                };
                for x in 0..width {
                    let offset = 3 * (source_y * reference.width + x * reference.width / width);
                    pixels.extend_from_slice(&reference.bgr[offset..offset + 3]);
                }
            }
            let capture = Frame {
                width,
                height,
                bgr: pixels,
            };
            let (normalized, rows) = capture.mapped_for_recognition(&cfg).unwrap();
            assert_eq!((normalized.width, normalized.height), (553, 1050));
            for y in [0., 32., 66., 500., 910., 1050.] {
                let bar = fixed_bar as f64;
                let capture_y = if y < bar {
                    y
                } else {
                    bar + (y - bar) * (height as f64 - bar) / (1050. - bar)
                };
                assert!(
                    (rows.fraction(y) * height as f64 - capture_y).abs() < 1e-6,
                    "{name} at {width}x{height}, y={y}, fixed={fixed_bar}"
                );
            }
            vision.reset_prior();
            let mut d = vision.detect(&normalized).unwrap();
            let actual = timing.detection(&d, None);
            if let Some(ms) = expected["hold_ms"].as_f64() {
                assert!(
                    (actual["hold_ms"].as_f64().unwrap() - ms).abs() < 8.0,
                    "{name}: {actual}"
                );
                map_detection_to_capture(&mut d, width as f64 / 553., rows);
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
        let timing = JumpTiming::initial(&cfg, &Options::parse_from(["jumpbot"]));
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let screen = Screen::new(&root.join("templates"), &cfg).unwrap();
        for backend in ["cpu", "simd"] {
            let mut vision = Vision::new(&cfg, backend).unwrap();
            for line in include_str!("../../tests/fixtures/v1.3-decisions.jsonl").lines() {
                let original: Value = serde_json::from_str(line).unwrap();
                let name = original["file"].as_str().unwrap();
                let mut target = original["target"].clone();
                // Independently measured bounding-box midpoints and ink areas in the retained PNGs.
                let mark = match name {
                    "purple-cylinder-stall.png" => Some((313.5, 549.5, 180)),
                    "skin-gold-cardboard-bonus.png" => Some((377.5, 513.5, 145)),
                    "skin-gold-cardboard.png" => Some((183.5, 504.5, 181)),
                    "skin-gold-red-platform.png" => Some((227.5, 530.5, 149)),
                    "skin-gold-round-platform.png" => Some((345.5, 532.0, 148)),
                    "skin-gold-tan-background.png" => Some((403.5, 499.0, 150)),
                    "skin-gold-yellow-badge.png" => Some((209.5, 520.0, 178)),
                    "skin-gold-yellow-platform.png" => Some((223.5, 528.0, 149)),
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
                let actual = timing.detection(&d, None);
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
