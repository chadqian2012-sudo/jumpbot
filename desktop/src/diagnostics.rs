//! Developer CLI for offline analysis of saved frames and score logs.
mod aim;
mod coverage;
mod pairs;

use crate::{
    asset_dir,
    regions::Mask,
    vision::{axis_components, face_center, Vision},
    Config, Frame,
};
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};
type Report<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(about = "离线视觉诊断（只读截图和跳跃记录）")]
struct Cli {
    /// Detection settings. Defaults to config.json beside the project assets.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Mask backend for saved frames; defaults to the configuration.
    #[arg(long, global=true, value_parser=["auto","cpu","simd","gpu","cuda","opencl"])]
    backend: Option<String>,
    #[command(subcommand)]
    task: Task,
}
#[derive(Subcommand)]
enum Task {
    /// Print block candidates and their geometry and selection gates.
    #[command(alias = "inspect_frame")]
    InspectFrame {
        image: PathBuf,
        #[arg(long)]
        mask: Option<PathBuf>,
        #[arg(long, default_value_t = 25)]
        top: usize,
        #[arg(long = "set", value_name = "KEY=VALUE")]
        overrides: Vec<String>,
    },
    /// Export plain estimates and center-dot observations as CSV.
    #[command(alias = "aim_error")]
    AimError {
        #[arg(long)]
        debug: Option<PathBuf>,
        #[arg(long, default_value = "run_*,miss,samples")]
        roots: String,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        max: usize,
        #[arg(long, value_parser=["estimate","geometric"])]
        mode: Option<String>,
    },
    /// Match logged jumps to saved miss screenshots and count center dots.
    #[command(alias = "dot_coverage")]
    DotCoverage {
        csv: PathBuf,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t=Class::LowRatio)]
        class: Class,
        #[arg(long, default_value_t = 0.45)]
        gate: f64,
        #[arg(long, default_value_t = 120)]
        min_width: i32,
        #[arg(long, default_value_t = 60.)]
        max_offset: f64,
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    /// Measure estimated and actual landing error using white dots.
    #[command(alias = "landing_truth")]
    LandingTruth {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long, default_value_t = 12.)]
        tol: f64,
        #[arg(long)]
        list: bool,
    },
    /// Compare mask and phase-correlation landing errors on recorded misses.
    #[command(alias = "measure_compare")]
    MeasureCompare {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long, default_value_t = 12.)]
        tol: f64,
    },
    /// Exercise settling and target confirmation with raw before/after pairs.
    #[command(alias = "replay_test")]
    ReplayTest {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long,default_value_t=12,value_parser=clap::value_parser!(u32).range(1..))]
        max: u32,
    },
}
#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum Class {
    #[value(name = "low_ratio", alias = "low-ratio")]
    LowRatio,
    Normal,
    Miss,
    All,
}

fn paths(base: &Path, pattern: &str) -> Report<Vec<PathBuf>> {
    let pattern = format!(
        "{}/{}",
        glob::Pattern::escape(&base.to_string_lossy()),
        pattern
    );
    let mut v = glob::glob(&pattern)?.collect::<Result<Vec<_>, _>>()?;
    v.sort();
    Ok(v)
}
fn name(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}
fn median(v: &[f64]) -> f64 {
    let mut sorted = v.to_vec();
    sorted.sort_by(f64::total_cmp);
    (sorted[(v.len() - 1) / 2] + sorted[v.len() / 2]) * 0.5
}
fn inspect(
    cfg: &mut Config,
    path: &Path,
    mask: Option<&Path>,
    top: usize,
    overrides: &[String],
) -> Report<bool> {
    for item in overrides {
        let (key, raw) = item.split_once('=').ok_or("expected KEY=VALUE")?;
        let v = cfg
            .0
            .get_mut(key)
            .ok_or_else(|| format!("unknown setting {key}"))?;
        *v = if v.is_string() {
            raw.into()
        } else {
            let parsed: serde_json::Value = serde_json::from_str(raw)?;
            if std::mem::discriminant(v) != std::mem::discriminant(&parsed) {
                return Err(format!("invalid type for {key}").into());
            }
            parsed
        };
    }
    cfg.validate()?;
    let frame = Frame::read(path)?;
    let d = Vision::new(cfg, cfg.s("masks_backend"))?.detect(&frame)?;
    let Some(piece) = d.piece else {
        println!("未找到棋子");
        return Ok(false);
    };
    let (w, h) = (frame.width as f64, frame.height as f64);
    let anchor = piece.y + cfg.f("piece_anchor_dy_ratio") * h;
    println!(
        "画面 {}x{} 棋子 {:?} 中心 ({:.1},{:.1}) 锚点_y={anchor:.1} tpl={:.3}",
        frame.width, frame.height, piece.bbox, piece.x, piece.y, piece.tpl_score
    );
    let as_mask = |raw: &[u8]| Mask::from_pixels(raw, frame.width, frame.height);
    let regions = as_mask(&d.block_mask).blocks(as_mask(&d.piece_mask));
    if let Some(p) = mask {
        image::save_buffer_with_format(
            p,
            &regions
                .labels
                .iter()
                .map(|&id| if id == 0 { 0 } else { 255 })
                .collect::<Vec<_>>(),
            frame.width as u32,
            frame.height as u32,
            image::ColorType::L8,
            image::ImageFormat::Png,
        )?;
    }
    let min_area = cfg.f("block_min_area_ratio") * w * h;
    let mut rows = Vec::new();
    for (id, &s) in regions.stats.iter().enumerate().skip(1) {
        let [bx, by, bw, bh, area] = s;
        if f64::from(area) < 0.4 * min_area
            || f64::from(area) > cfg.f("block_max_area_ratio") * w * h
        {
            continue;
        }
        let Some((x, y, tw)) = face_center(&regions, id, s, cfg.f("axis_slope")) else {
            continue;
        };
        let (_, along, cross) = axis_components(x - piece.x, y - piece.y, cfg.f("axis_slope"));
        let dist = (x - piece.x).hypot(y - anchor);
        let up = y < piece.y - cfg.f("target_min_up_ratio") * h;
        let range =
            (cfg.f("min_jump_dist_ratio") * w..=cfg.f("max_jump_dist_ratio") * w).contains(&along);
        let size = f64::from(area) >= min_area
            && f64::from(bw) >= cfg.f("block_min_width_ratio") * w
            && f64::from(bh) >= cfg.f("block_min_height_ratio") * h;
        rows.push((dist,format!("bbox=({bx},{by},{bw},{bh}) area={area} center=({x:.1},{y:.1}) top_y={by} top_w={tw:.1} along={along:.1} cross={cross:.1} ratio={:.2} dist={dist:.1} up={up} in_range={range} size_ok={size}",cross/along.abs().max(1.))));
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("候选 {} 个", rows.len());
    for (_, s) in rows.into_iter().take(top) {
        println!("{s}")
    }
    if let Some(t) = d.target {
        let (_, a, c) = axis_components(t.x - piece.x, t.y - piece.y, cfg.f("axis_slope"));
        println!(
            "最终选择 ({:.1},{:.1}) bbox={:?} along={a:.1} cross={c:.1} ratio={:.2} dist={:.1}",
            t.x,
            t.y,
            t.bbox,
            c / a.abs().max(1.),
            (t.x - piece.x).hypot(t.y - anchor)
        );
    } else {
        println!("最终选择 无目标")
    }
    Ok(true)
}

fn dispatch(cli: Cli) -> Report<bool> {
    let root = asset_dir();
    let mut cfg = Config::load(&cli.config.unwrap_or_else(|| root.join("config.json")))?;
    if let Some(backend) = cli.backend {
        cfg.0["masks_backend"] = backend.into();
    }
    let debug = root.join("debug");
    let misses = debug.join("miss");
    match cli.task {
        Task::InspectFrame {
            image,
            mask,
            top,
            overrides,
        } => inspect(&mut cfg, &image, mask.as_deref(), top, &overrides),
        Task::AimError {
            debug: dir,
            roots,
            out,
            max,
            mode,
        } => {
            if let Some(mode) = mode {
                cfg.0["aim_center_mode"] = mode.into()
            }
            aim::run(
                &cfg,
                &dir.unwrap_or_else(|| debug.clone()),
                &roots,
                max,
                &out.unwrap_or_else(|| debug.join("aim/dataset.csv")),
            )?;
            Ok(true)
        }
        Task::DotCoverage {
            csv,
            dir,
            class,
            gate,
            min_width,
            max_offset,
            limit,
        } => {
            cfg.0["center_dot_ratio_gate"] = gate.into();
            cfg.0["center_dot_min_block_width"] = min_width.into();
            cfg.0["center_dot_max_offset_px"] = max_offset.into();
            coverage::run(&cfg, &csv, &dir.unwrap_or(misses), class, limit)?;
            Ok(true)
        }
        Task::LandingTruth { dir, tol, list } => {
            pairs::measure(&cfg, &dir.unwrap_or(misses), tol, Some(list))
        }
        Task::MeasureCompare { dir, tol } => {
            pairs::measure(&cfg, &dir.unwrap_or(misses), tol, None)
        }
        Task::ReplayTest { dir, max } => pairs::replay(&cfg, &debug, dir.as_deref(), max as usize),
    }
}
pub fn run() -> ExitCode {
    if let Some(result) = crate::runtime::opencl_preflight_command() {
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    match dispatch(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
