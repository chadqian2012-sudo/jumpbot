mod calibration;
mod control;
pub mod diagnostics;
mod harvest;
mod landing;
mod masks;
mod options;
mod platform;
mod play;
pub mod profile;
mod records;
mod regions;
pub mod runtime;
mod screen;
mod skin;
mod template;
mod vision;

use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub bgr: Vec<u8>,
}
impl Frame {
    pub fn valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= i32::MAX as usize
            && self.height <= i32::MAX as usize
            && self
                .width
                .checked_mul(self.height)
                .and_then(|n| n.checked_mul(3))
                == Some(self.bgr.len())
    }
}

pub fn asset_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("Windows executable path");
    exe.ancestors()
        .skip(1)
        .find(|p| {
            p == &exe.parent().unwrap() && p.join("templates").is_dir()
                || p.join("desktop/Cargo.toml").is_file() && p.join("templates").is_dir()
        })
        .unwrap_or_else(|| exe.parent().unwrap())
        .to_path_buf()
}

#[derive(Clone)]
pub struct Config(pub Value);
impl Default for Config {
    fn default() -> Self {
        Self(serde_json::from_str(include_str!("../../config.json")).expect("bundled config"))
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg = Self::default();
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let user: Value =
            serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        for (k, v) in user.as_object().ok_or("config.json 需要一个对象")? {
            let Some(default) = cfg.0.get(k) else {
                eprintln!("忽略未知配置项 {k}");
                continue;
            };
            let valid = match default {
                Value::Number(_) => v.is_number(),
                Value::Bool(_) => v.is_boolean(),
                Value::String(_) => v.is_string(),
                Value::Array(a) => v.as_array().is_some_and(|b| {
                    b.len() == a.len()
                        && b.iter()
                            .all(|x| x.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n)))
                }),
                _ => false,
            };
            if !valid {
                return Err(format!("配置项 {k} 类型或范围不正确"));
            }
            cfg.0[k] = v.clone();
        }
        cfg.validate()?;
        Ok(cfg)
    }
    pub fn validate(&self) -> Result<()> {
        let dim = |key: &str| {
            self.0[key]
                .as_u64()
                .filter(|n| *n > 0 && *n <= i32::MAX as u64)
        };
        let pixels = dim("window_ref_width")
            .zip(dim("window_ref_height"))
            .map(|(w, h)| w * h);
        if pixels.is_none_or(|n| n > 32 * 1024 * 1024)
            || self.0["window_size_tol_px"].as_u64().is_none()
        {
            return Err(
                "标定宽高必须为正整数，总像素不能超过 33554432，尺寸容差必须为非负整数".into(),
            );
        }
        for key in [
            "startup_wait_s",
            "start_delay_s",
            "window_wait_s",
            "max_runtime_s",
            "settle_timeout_s",
            "acquire_poll_ms",
            "settle_poll_ms",
        ] {
            if self.f(key) < 0.0 {
                return Err(format!("{key} 必须非负"));
            }
        }
        if self.0["max_jumps"].as_u64().is_none()
            || self.f("press_duration_min_ms") <= 0.
            || self.f("press_duration_max_ms") < self.f("press_duration_min_ms")
            || self.i("detect_retry_limit") < 1
        {
            return Err("次数或按压区间无效".into());
        }
        for (lo, hi) in [
            ("min_k_ratio", "max_k_ratio"),
            ("piece_min_area_ratio", "piece_max_area_ratio"),
            ("min_jump_dist_ratio", "max_jump_dist_ratio"),
        ] {
            if self.f(lo) < 0. || self.f(lo) > self.f(hi) {
                return Err(format!("{lo} 超出 {hi}"));
            }
        }
        for key in [
            "max_step_ratio",
            "residual_trim_ratio",
            "k",
            "gamma",
            "score_reread_first_ms",
            "restart_wait_s",
        ] {
            if self.f(key) < 0. {
                return Err(format!("{key} 必须非负"));
            }
        }
        for key in [
            "press_duration_max_ms",
            "score_reread_first_ms",
            "settle_poll_ms",
            "acquire_poll_ms",
            "start_delay_s",
            "startup_wait_s",
            "window_wait_s",
            "max_runtime_s",
            "settle_timeout_s",
        ] {
            if std::time::Duration::try_from_secs_f64(self.f(key)).is_err() {
                return Err(format!("{key} 超出计时范围"));
            }
        }
        Ok(())
    }
    pub fn f(&self, k: &str) -> f64 {
        self.0[k].as_f64().expect(k)
    }
    pub fn i(&self, k: &str) -> i64 {
        self.f(k) as i64
    }
    pub fn b(&self, k: &str) -> bool {
        self.0[k].as_bool().expect(k)
    }
    pub fn s(&self, k: &str) -> &str {
        self.0[k].as_str().expect(k)
    }
    pub fn at(&self, k: &str, n: usize) -> f64 {
        self.0[k][n].as_f64().expect(k)
    }
}

pub(crate) fn update_config<const N: usize>(
    path: &Path,
    updates: [(&str, Value); N],
) -> Result<()> {
    use std::io::Write;
    let mut config: Value = match fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default().0,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let fields = config.as_object_mut().ok_or("config.json 需要一个对象")?;
    for (key, value) in updates {
        fields.insert(key.to_owned(), value);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, &config).map_err(|e| e.to_string())?;
    file.write_all(b"\n")
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
