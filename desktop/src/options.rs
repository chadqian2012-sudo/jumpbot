use crate::{Config, Result};
use clap::Parser;
use serde_json::json;
use std::{num::NonZeroUsize, path::PathBuf};

#[derive(Parser)]
#[command(name = "JumpBot", about = "Windows 跳一跳控制与离线识别")]
pub(crate) struct Options {
    #[arg(long, hide = true)]
    pub worker: bool,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long,value_parser=["auto","gpu","cuda","opencl","simd","cpu"])]
    pub backend: Option<String>,
    #[arg(long)]
    pub no_cuda: bool,
    #[arg(long)]
    pub no_opencl: bool,
    #[arg(long)]
    pub opencl_device: Option<String>,
    #[arg(long)]
    pub pause_hotkey: Option<String>,
    #[arg(long)]
    pub startup_wait_s: Option<f64>,
    #[arg(long)]
    pub start_delay_s: Option<f64>,
    #[arg(long)]
    pub k: Option<f64>,
    #[arg(long)]
    pub gamma: Option<f64>,
    #[arg(long)]
    pub no_adaptive: bool,
    #[arg(long)]
    pub max_jumps: Option<u64>,
    #[arg(long)]
    pub selftest: bool,
    #[arg(long)]
    pub image: Vec<String>,
    /// Warm up once, then time this many detections per image.
    #[arg(long)]
    pub bench_repeat: Option<NonZeroUsize>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub probe: bool,
    #[arg(long)]
    pub no_activate: bool,
    #[arg(long)]
    pub out_dir: Option<PathBuf>,
    #[arg(long)]
    pub log_dir: Option<PathBuf>,
    #[arg(long)]
    pub background: bool,
    #[arg(long,default_value="screen",value_parser=["screen","printwindow"])]
    pub capture: String,
    #[arg(long)]
    pub no_dashboard: bool,
    #[arg(long)]
    pub daemon: bool,
    #[arg(long, default_value_t = 0.0)]
    pub daemon_wait_s: f64,
    #[arg(long, default_value_t = 0)]
    pub daemon_max_rounds: u32,
    #[arg(long)]
    pub save_debug: bool,
    #[arg(long)]
    pub persist_best_k: bool,
    #[arg(long, default_value_t = 0)]
    pub samples: usize,
    #[arg(long, default_value_t = 0.35)]
    pub sample_interval: f64,
    #[arg(long)]
    pub click_xy: Option<String>,
    #[arg(long)]
    pub jump_test: bool,
    #[arg(long, default_value_t = 0.0)]
    pub hold_ms: f64,
    #[arg(long, default_value_t = 8)]
    pub burst: usize,
    #[arg(long, default_value_t = 0.25)]
    pub burst_interval: f64,
    #[arg(long)]
    pub calibrate: bool,
    #[arg(long)]
    pub persist_calibration: bool,
    #[arg(long, default_value_t = 20)]
    pub calib_per_band: usize,
    #[arg(long, value_delimiter = ',', default_value = "0.9,1.0,1.1")]
    pub calib_k_scales: Vec<f64>,
    #[arg(long)]
    pub bias_sweep: bool,
    #[arg(long, default_value_t = 100)]
    pub bias_per_level: usize,
    #[arg(long, value_delimiter = ',', default_value = "0,2,-2,4,-4,6,-6")]
    pub bias_levels: Vec<f64>,
    #[arg(long)]
    pub harvest_score: bool,
    #[arg(long)]
    pub score_dir: Option<PathBuf>,
}
impl Options {
    pub fn apply(&self, c: &mut Config) -> Result<()> {
        for (k, v) in [
            ("k", self.k),
            ("gamma", self.gamma),
            ("start_delay_s", self.start_delay_s),
            ("startup_wait_s", self.startup_wait_s),
        ] {
            if let Some(v) = v {
                if !v.is_finite() || v < 0. {
                    return Err(format!("{k} 必须是非负有限数"));
                }
                c.0[k] = json!(v)
            }
        }
        for n in [
            self.hold_ms,
            self.burst_interval,
            self.sample_interval,
            self.daemon_wait_s,
        ] {
            if !n.is_finite() || n < 0. {
                return Err("时间参数必须是非负有限数".into());
            }
        }
        if let Some(v) = self.max_jumps {
            c.0["max_jumps"] = json!(v)
        }
        if let Some(v) = &self.pause_hotkey {
            c.0["pause_hotkey"] = json!(v)
        }
        if self.no_adaptive {
            c.0["gamma"] = json!(0)
        }
        if self.no_cuda {
            c.0["cuda_enabled"] = json!(false)
        }
        if self.no_opencl {
            c.0["opencl_enabled"] = json!(false)
        }
        if let Some(v) = &self.opencl_device {
            c.0["opencl_device"] = json!(v)
        }
        let backend = self
            .backend
            .clone()
            .or_else(|| std::env::var("JUMPBOT_MASKS_BACKEND").ok());
        if let Some(v) = backend {
            c.0["masks_backend"] = json!(v)
        }
        if !matches!(
            c.s("masks_backend"),
            "auto" | "gpu" | "cuda" | "opencl" | "simd" | "cpu"
        ) {
            return Err("未知后端".into());
        }
        if self.burst == usize::MAX
            || self
                .calib_k_scales
                .iter()
                .any(|v| !v.is_finite() || *v <= 0.)
            || self.bias_levels.iter().any(|v| !v.is_finite())
            || (self.calibrate && self.calib_k_scales.is_empty())
            || (self.bias_sweep
                && (self.bias_levels.is_empty() || self.bias_levels.iter().any(|v| *v <= -100.)))
        {
            return Err("采样数量或标定档位无效".into());
        }
        for value in [
            self.sample_interval,
            self.burst_interval,
            self.hold_ms,
            self.daemon_wait_s,
        ] {
            if std::time::Duration::try_from_secs_f64(value).is_err() {
                return Err("时间参数超出计时范围".into());
            }
        }
        c.validate()
    }
}
