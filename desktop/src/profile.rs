//! Offline timing and bitwise GPU validation. This module never creates a window or input controller.
use crate::{
    asset_dir, landing, masks::Masks, screen::Screen, vision::Vision, Config, Frame, Result,
};
use clap::Parser;
use serde_json::json;
use std::{hint::black_box, path::PathBuf, time::Instant};

#[derive(Parser)]
#[command(about = "从文件测量识别耗时，并与 CPU 逐像素对比")]
struct Options {
    #[arg(long, required = true)]
    image: Vec<String>,
    #[arg(long, default_value="simd",value_parser=["cpu","simd","cuda","opencl","auto"])]
    backend: String,
    #[arg(long)]
    opencl_device: Option<String>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value_t=7,value_parser=clap::value_parser!(u32).range(1..))]
    repeat: u32,
    /// Only time masks. Suitable for a multi-thousand-frame CUDA buffer stress run.
    #[arg(long)]
    masks_only: bool,
}

fn median(samples: &mut [f64]) -> f64 {
    samples.sort_unstable_by(f64::total_cmp);
    (samples[(samples.len() - 1) / 2] + samples[samples.len() / 2]) * 0.5
}
fn timed<T>(run: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let value = black_box(run());
    (value, t.elapsed().as_secs_f64() * 1000.)
}

pub fn run() -> Result<()> {
    if let Some(result) = crate::runtime::opencl_preflight_command() {
        return result;
    }
    let opt = Options::parse();
    let mut cfg = opt
        .config
        .as_deref()
        .map(Config::load)
        .transpose()?
        .unwrap_or_default();
    if let Some(name) = opt.opencl_device {
        cfg.0["opencl_device"] = name.into();
    }
    let screen = Screen::new(&asset_dir().join("templates"), &cfg)?;
    let mut masks = Masks::new(&cfg, &opt.backend);
    let mut cpu = Masks::new(&cfg, "cpu");
    let mut vision = Vision::new(&cfg, &opt.backend)?;
    for pattern in opt.image {
        let paths: Vec<_> = if std::path::Path::new(&pattern).is_file() {
            vec![PathBuf::from(&pattern)]
        } else {
            glob::glob(&pattern)
                .map_err(|e| e.to_string())?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        if paths.is_empty() {
            return Err(format!("没有匹配画面 {pattern}"));
        }
        for path in paths {
            let f = Frame::read(&path)?.for_recognition(&cfg)?;
            let expected = cpu.compute(&f)?;
            let mut mask_ms = Vec::new();
            let mut detect_ms = Vec::new();
            let mut hud_ms = Vec::new();
            let mut motion_ms = Vec::new();
            let mut landing_ms = Vec::new();
            for i in 0..=opt.repeat {
                let (result, t) = timed(|| masks.compute(&f));
                if result? != expected {
                    return Err(format!("{} frame {i} differs from CPU", path.display()));
                }
                if opt.backend != "auto" && masks.backend() != opt.backend {
                    return Err(format!(
                        "requested {}, ran {}",
                        opt.backend,
                        masks.backend()
                    ));
                }
                if i > 0 {
                    mask_ms.push(t);
                }
                if opt.masks_only {
                    continue;
                }
                vision.reset_prior();
                let (detected, detect) = timed(|| vision.detect(&f));
                let d = detected?;
                let (_, score) = timed(|| screen.score(&f, &cfg));
                let (sample, motion) = timed(|| f.motion_sample(&cfg));
                sample?;
                let (_, foot) = timed(|| landing::foot(&f, &d));
                if i > 0 {
                    detect_ms.push(detect);
                    hud_ms.push(score);
                    motion_ms.push(motion);
                    landing_ms.push(foot);
                }
            }
            let mut row = json!({"image":path,"size":[f.width,f.height],"backend":masks.backend(),"mask_equal":true,"samples":opt.repeat,"mask_ms":median(&mut mask_ms)});
            if !opt.masks_only {
                row["detect_ms"] = median(&mut detect_ms).into();
                row["hud_ms"] = median(&mut hud_ms).into();
                row["motion_sample_ms"] = median(&mut motion_ms).into();
                row["foot_check_ms"] = median(&mut landing_ms).into();
            }
            println!("{row}");
        }
    }
    Ok(())
}
