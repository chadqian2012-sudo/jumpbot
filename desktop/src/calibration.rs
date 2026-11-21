use crate::{platform::capture_bgr, Config, Result};
use chrono::Local;
use serde_json::json;
use std::{fs, path::Path, ptr::null_mut};
use windows_sys::Win32::Graphics::Gdi::{
    Ellipse, GetStockObject, LineTo, MoveToEx, PatBlt, SelectObject, SetBkMode, SetDCBrushColor,
    SetDCPenColor, TextOutA, DC_BRUSH, DC_PEN, TRANSPARENT, WHITENESS,
};

type Sample = (f64, f64, f64, f64);

pub(crate) fn finish(
    samples: &[Sample],
    cfg: &Config,
    config_path: &Path,
    log_dir: &Path,
    persist: bool,
) -> Result<()> {
    if samples.len() < 8 {
        eprintln!("标定样本只有 {} 个，至少需要 8 个。", samples.len());
        return Ok(());
    }
    if samples
        .iter()
        .any(|&(t, l, d, k)| ![t, l, d, k].iter().all(|v| v.is_finite()))
    {
        return Err("标定样本含有非有限数值。".into());
    }
    let n = samples.len() as f64;
    let mean_t = samples.iter().map(|s| s.0).sum::<f64>() / n;
    let mean_l = samples.iter().map(|s| s.1).sum::<f64>() / n;
    let mut tt = 0.0;
    let mut tl = 0.0;
    let mut ll = 0.0;
    let (mut tmin, mut tmax, mut lmin, mut lmax) =
        (samples[0].0, samples[0].0, samples[0].1, samples[0].1);
    for &(t, l, _, _) in samples {
        tt += (t - mean_t).powi(2);
        tl += (t - mean_t) * (l - mean_l);
        ll += (l - mean_l).powi(2);
        tmin = tmin.min(t);
        tmax = tmax.max(t);
        lmin = lmin.min(l);
        lmax = lmax.max(l);
    }
    // For a single sampled duration, use the minimum-norm least-squares solution.
    let a = if tmin == tmax {
        mean_l * mean_t / (mean_t * mean_t + 1.0)
    } else {
        tl / tt
    };
    let c = mean_l - a * mean_t;
    let residual = |t, l| l - (a * t + c);
    let squared_error = samples
        .iter()
        .map(|&(t, l, _, _)| residual(t, l).powi(2))
        .sum::<f64>();
    let r2 = 1.0 - squared_error / ll.max(1e-9);
    let std_px = (squared_error / n).sqrt();
    let k = cfg.f("k");
    let implied_k = if a > 1e-6 { 1.0 / a } else { f64::INFINITY };
    let credit = samples.len() >= 25
        && r2 >= 0.90
        && k > 0.0
        && (0.8..=1.25).contains(&(implied_k / k))
        && lmax - lmin >= 150.0;
    eprintln!(
        "标定拟合 L = {a:.4} * t + {c:.1}，R²={r2:.4}，残差标准差 {std_px:.1} px，共 {} 个样本。",
        samples.len()
    );
    if a > 1e-6 {
        eprintln!("等效 k={implied_k:.4} ms/px，截距 {:.1} ms。", -c / a);
    }

    fs::create_dir_all(log_dir).map_err(|e| e.to_string())?;
    let base = log_dir.join(format!(
        "calib_{}",
        Local::now().format("%Y%m%d_%H%M%S_%6f")
    ));
    let csv_path = base.with_extension("csv");
    let mut data = String::from("\u{feff}hold_ms,achieved_px,target_px,k_used,residual_px\r\n");
    use std::fmt::Write as _;
    for &(t, l, d, k_used) in samples {
        write!(
            data,
            "{t:.1},{l:.1},{d:.1},{k_used:.3},{:.1}\r\n",
            residual(t, l)
        )
        .unwrap();
    }
    fs::write(&csv_path, data).map_err(|e| format!("{}: {e}", csv_path.display()))?;

    let plot = base.with_extension("png");
    let (w, h, pad) = (760, 500, 52);
    let position = |t: f64, l: f64| {
        (
            (pad as f64 + (t - tmin) * f64::from(w - 2 * pad) / (tmax - tmin).max(1e-6)) as i32,
            (f64::from(h - pad) - (l - lmin) * f64::from(h - 2 * pad) / (lmax - lmin).max(1e-6))
                as i32,
        )
    };
    capture_bgr(w, h, |dc| unsafe {
        if PatBlt(dc, 0, 0, w, h, WHITENESS) == 0 {
            return Err("标定图画布初始化失败。".into());
        }
        SelectObject(dc, GetStockObject(DC_PEN));
        SelectObject(dc, GetStockObject(DC_BRUSH));
        let line = |(x, y), (ex, ey)| {
            MoveToEx(dc, x, y, null_mut());
            LineTo(dc, ex, ey);
        };
        SetDCPenColor(dc, 0xc8c8c8);
        line((pad, pad), (pad, h - pad));
        line((pad, h - pad), (w - pad, h - pad));
        SetDCPenColor(dc, 0xb45a28);
        SetDCBrushColor(dc, 0xb45a28);
        for &(t, l, _, _) in samples {
            let (x, y) = position(t, l);
            Ellipse(dc, x - 4, y - 4, x + 5, y + 5);
        }
        SetDCPenColor(dc, 0xc83c00);
        line(position(tmin, a * tmin + c), position(tmax, a * tmax + c));
        SetBkMode(dc, TRANSPARENT as i32);
        let label = |x, y, text: &str| TextOutA(dc, x, y, text.as_ptr(), text.len() as i32);
        label(pad, 6, &format!("L = {a:.4} * t + {c:.1}     R2 = {r2:.4}"));
        label(
            pad,
            28,
            &format!(
                "n={}   t: {tmin:.0}..{tmax:.0} ms   L: {lmin:.0}..{lmax:.0} px",
                samples.len()
            ),
        );
        label(w / 2 - 60, h - 26, "press time (ms)");
        label(8, h / 2, "jump distance (px)");
        Ok(())
    })?
    .write(&plot)?;
    eprintln!(
        "标定数据 {}\n标定散点图 {}",
        csv_path.display(),
        plot.display()
    );
    if !credit {
        eprintln!("拟合未通过可信度检查，保留原配置。");
    } else if !persist {
        eprintln!("落点误差测量仅供参考。显式使用 --persist-calibration 才会保存拟合值。");
    } else {
        crate::update_config(
            config_path,
            [
                ("transfer_a", json!((a * 1e4).round_ties_even() / 1e4)),
                ("transfer_c", json!((c * 1e2).round_ties_even() / 1e2)),
            ],
        )?;
        eprintln!("已保存 transfer_a={:.4}，transfer_c={:.2}。", a, c);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_reports_fit_and_only_persists_a_credible_opt_in() -> Result<()> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let cfg = Config::default();
        let path = dir.path().join("config.json");
        let logs = dir.path().join("logs");
        let original = r#"{"preserve_me":"user data","transfer_a":0,"transfer_c":0}"#;
        fs::write(&path, original).unwrap();
        let sample: Vec<Sample> = (0..30)
            .map(|i| {
                let t = 200.0 + f64::from(i) * 20.0;
                let distance = 0.4 * t - 12.0;
                (t, distance, distance + 4.0, 2.5)
            })
            .collect();

        finish(&sample[..7], &cfg, &path, &logs, true)?;
        assert!(!logs.exists());
        finish(&sample[..24], &cfg, &path, &logs, true)?;
        finish(&sample, &cfg, &path, &logs, false)?;
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        let files: Vec<_> = fs::read_dir(&logs)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 4);
        for file in files {
            if file.extension().unwrap() == "png" {
                let plot = crate::Frame::read(&file)?;
                assert_eq!((plot.width, plot.height), (760, 500));
                assert!(plot.bgr.as_chunks::<3>().0.contains(&[180, 90, 40]));
            } else {
                let csv = fs::read_to_string(file).unwrap();
                assert!(
                    csv.starts_with("\u{feff}hold_ms,achieved_px,target_px,k_used,residual_px\r\n")
                );
                assert!(csv.contains("200.0,68.0,72.0,2.500,0.0"));
            }
        }
        finish(&sample, &cfg, &path, &logs, true)?;
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({"preserve_me":"user data","transfer_a":0.4,"transfer_c":-12.0})
        );
        Ok(())
    }
}
