use crate::{Config, Frame};
#[cfg(any(feature = "cuda", feature = "opencl"))]
use std::{
    env,
    time::{Duration, Instant},
};

#[cfg(target_arch = "x86_64")]
#[path = "../native/masks_avx2.rs"]
mod avx2;
#[cfg(any(feature = "cuda", feature = "opencl"))]
#[path = "../native/masks_gpu.rs"]
mod gpu;

type Pair = (Vec<u8>, Vec<u8>);
type Tables = [i32; 526]; // OpenCV saturation and hue divisors, then 14 mask thresholds.

fn tables(cfg: &Config) -> Tables {
    let mut table = [0; 526];
    for i in 1..256 {
        table[i] = ((255 << 12) as f64 / i as f64).round_ties_even() as i32;
        table[256 + i] = ((180 << 12) as f64 / (6 * i) as f64).round_ties_even() as i32;
    }
    let keys = [
        "block_color_delta",
        "block_dark_delta",
        "block_hue_delta",
        "block_hue_sat_min",
        "block_sat_delta",
        "block_pale_delta",
        "block_white_sat_max",
        "block_white_val_min",
        "block_bright_delta",
        "piece_sat_min",
        "piece_val_max",
        "piece_hue_min",
        "piece_hue_max",
    ];
    for (i, key) in keys.iter().enumerate() {
        let v = cfg.f(key);
        table[512 + i] = if matches!(i, 6 | 10 | 11) {
            v.ceil()
        } else {
            v.floor()
        } as i32;
    }
    table[525] = i32::from(cfg.f("block_bright_delta") > 0.);
    table
}

fn background(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width, frame.height);
    let band = (0.03 * w as f64).round_ties_even().max(3.) as usize;
    // NumPy's right slice starts at w-band; for w<band a negative start wraps.
    let right_start = if band <= w {
        w - band
    } else {
        (2 * w).saturating_sub(band)
    };
    let mut left = vec![0f32; 3 * h];
    let mut right = vec![0f32; 3 * h];
    jumpbot_vision::edge_medians(
        &frame.bgr,
        w,
        band.min(w),
        right_start,
        &mut left,
        &mut right,
    );
    let mut count = 0.;
    let (mut sy, mut syy) = (0., 0.);
    let (mut sums, mut products) = ([0.; 3], [0.; 3]);
    for (y, (l, r)) in left
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(right.as_chunks::<3>().0)
        .enumerate()
    {
        let agree = (0..3).map(|c| (l[c] - r[c]).abs()).sum::<f32>() < 36.;
        let y = y as f64;
        for c in 0..3 {
            l[c] = (l[c] + r[c]) * 0.5;
        }
        if agree {
            count += 1.;
            sy += y;
            syy += y * y;
            for c in 0..3 {
                sums[c] += l[c] as f64;
                products[c] += y * l[c] as f64;
            }
        }
    }
    let mut intercept = [0.; 3];
    let mut slope = [0.; 3];
    if count >= 20. {
        for c in 0..3 {
            slope[c] = (count * products[c] - sy * sums[c]) / (count * syy - sy * sy);
            intercept[c] = (sums[c] - slope[c] * sy) / count;
        }
    } else {
        for c in 0..3 {
            let mut values: Vec<_> = left.as_chunks::<3>().0.iter().map(|row| row[c]).collect();
            let mid = h / 2;
            let (lo, hi, _) = values.select_nth_unstable_by(mid, f32::total_cmp);
            intercept[c] = if h % 2 == 0 {
                (*hi + lo.iter().copied().fold(0., f32::max)) * 0.5
            } else {
                *hi
            } as f64;
        }
    }
    let mut bg = vec![0; h * 3];
    for (y, row) in bg.as_chunks_mut::<3>().0.iter_mut().enumerate() {
        for c in 0..3 {
            row[c] = (intercept[c] + slope[c] * y as f64) as f32 as u8;
        }
    }
    bg
}

fn hsv(p: &[u8], table: &Tables) -> [i32; 3] {
    let (b, g, r) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
    let v = r.max(g).max(b);
    let d = v - r.min(g).min(b);
    let h = if v == r {
        g - b
    } else if v == g {
        b - r + 2 * d
    } else {
        r - g + 4 * d
    };
    let h = (h * table[256 + d as usize] + 2048) >> 12;
    [
        if h < 0 { h + 180 } else { h },
        (d * table[v as usize] + 2048) >> 12,
        v,
    ]
}

fn scalar_row(
    row: &[u8],
    bg: &[u8],
    base: [i32; 3],
    table: &Tables,
    piece: &mut [u8],
    block: &mut [u8],
) {
    let t = &table[512..];
    let [bh, bs, bv] = base;
    for ((p, piece), block) in row.as_chunks::<3>().0.iter().zip(piece).zip(block) {
        let [h, s, v] = hsv(p, table);
        let diff = (0..3).map(|c| i32::from(p[c].abs_diff(bg[c]))).sum::<i32>();
        *piece = 255
            * u8::from(
                ((s > t[9] && v < t[10]) || (v < 110 && s > 25)) && h >= t[11] && h <= t[12],
            );
        *block = 255
            * u8::from(
                bv - v > t[1]
                    || ((h - bh).abs() > t[2] && s > t[3])
                    || s - bs > t[4]
                    || bs - s > t[5]
                    || (s < t[6] && v > t[7])
                    || diff > t[0]
                    || (t[13] != 0 && v - bv > t[8] && s <= bs),
            );
    }
}

fn cpu(frame: &Frame, bg: &[u8], table: &Tables, rows: [usize; 2], simd: bool) -> Pair {
    let w = frame.width;
    let mut pair = (vec![0; w * frame.height], vec![0; w * frame.height]);
    for y in rows[0]..rows[1] {
        let row = &frame.bgr[3 * w * y..3 * w * (y + 1)];
        let color = &bg[3 * y..3 * y + 3];
        let base = hsv(color, table);
        let piece = &mut pair.0[w * y..w * (y + 1)];
        let block = &mut pair.1[w * y..w * (y + 1)];
        #[cfg(target_arch = "x86_64")]
        if simd {
            // simd can only be set after AVX2 runtime detection.
            unsafe {
                avx2::row(row, color, base, table, piece, block);
            }
            continue;
        }
        scalar_row(row, color, base, table, piece, block);
    }
    pair
}

#[derive(Copy, Clone, PartialEq)]
enum Mode {
    Auto,
    Cpu,
    Simd,
    Cuda,
    Opencl,
    Unknown,
}

pub struct Masks {
    mode: Mode,
    simd: bool,
    ratios: [f64; 2],
    table: Tables,
    last: &'static str,
    #[cfg(any(feature = "cuda", feature = "opencl"))]
    gpu: GpuSelection,
}

#[cfg(any(feature = "cuda", feature = "opencl"))]
struct GpuSelection {
    cuda: bool,
    opencl: bool,
    selector: String,
    // None until the first real frame can be used for device validation.
    devices: Option<Vec<gpu::Device>>,
    chosen: Option<usize>,
    calls: u64,
    cpu_ms: f64,
    samples: Vec<f64>,
}

impl Masks {
    pub fn new(cfg: &Config, backend: &str) -> Self {
        let backend = if backend.is_empty() {
            cfg.s("masks_backend")
        } else {
            backend
        };
        let mode = match backend {
            "auto" => Mode::Auto,
            "cpu" => Mode::Cpu,
            "simd" => Mode::Simd,
            "gpu" | "cuda" => Mode::Cuda,
            "opencl" => Mode::Opencl,
            _ => Mode::Unknown,
        };
        #[cfg(target_arch = "x86_64")]
        let simd = mode != Mode::Cpu && cfg.b("masks_simd") && is_x86_feature_detected!("avx2");
        #[cfg(not(target_arch = "x86_64"))]
        let simd = false;
        #[cfg(any(feature = "cuda", feature = "opencl"))]
        let enabled =
            |key: &str, disable: &str| cfg.b(key) && env::var(disable).as_deref() != Ok("1");
        Self {
            mode,
            simd,
            ratios: [cfg.f("top_bar_ratio"), 1. - cfg.f("bottom_bar_ratio")],
            table: tables(cfg),
            last: if simd { "simd" } else { "cpu" },
            #[cfg(any(feature = "cuda", feature = "opencl"))]
            gpu: GpuSelection {
                cuda: cfg!(feature = "cuda") && enabled("cuda_enabled", "JUMPBOT_DISABLE_CUDA"),
                opencl: cfg!(feature = "opencl")
                    && enabled("opencl_enabled", "JUMPBOT_DISABLE_OPENCL"),
                selector: if cfg.s("opencl_device").is_empty() {
                    env::var("JUMPBOT_OPENCL_DEVICE").unwrap_or_default()
                } else {
                    cfg.s("opencl_device").into()
                },
                devices: None,
                chosen: None,
                calls: 0,
                cpu_ms: 0.,
                samples: Vec::new(),
            },
        }
    }

    pub fn backend(&self) -> &str {
        self.last
    }

    #[cfg(any(feature = "cuda", feature = "opencl"))]
    fn select(&mut self, f: &Frame, rows: [usize; 2]) {
        if self.gpu.devices.is_none() {
            let mut devices = Vec::new();
            if self.gpu.cuda && matches!(self.mode, Mode::Auto | Mode::Cuda) {
                match gpu::Device::cuda(&self.table) {
                    Ok(d) => devices.push(d),
                    Err(e) => eprintln!("CUDA: {e}"),
                }
            }
            if self.gpu.opencl && matches!(self.mode, Mode::Auto | Mode::Opencl) {
                devices.extend(gpu::opencl_devices(&self.gpu.selector, &self.table));
            }
            self.gpu.devices = Some(devices);
        }
        self.gpu.chosen = None;
        self.gpu.samples.clear();
        let devices = self.gpu.devices.as_mut().unwrap();
        if devices.is_empty() {
            return;
        }
        let bg = background(f);
        let expected = cpu(f, &bg, &self.table, rows, false);
        let mut best = f64::INFINITY;
        if self.mode == Mode::Auto {
            self.gpu.cpu_ms =
                benchmark(|| Ok(cpu(f, &background(f), &self.table, rows, self.simd))).unwrap();
            best = self.gpu.cpu_ms / 1.5;
            eprintln!(
                "{}: {:.3} ms",
                if self.simd { "SIMD" } else { "CPU" },
                self.gpu.cpu_ms
            );
        }
        devices.retain_mut(|device| {
            let result = device.compute(f, &bg, rows).and_then(|got| {
                if got != expected {
                    return Err("mask mismatch against CPU".into());
                }
                benchmark(|| device.compute(f, &background(f), rows))
            });
            match result {
                Ok(ms) => {
                    device.ms = ms;
                    eprintln!("{}: {ms:.3} ms", device.name);
                    true
                }
                Err(e) => {
                    eprintln!("{}: {e}", device.name);
                    false
                }
            }
        });
        for (index, device) in devices.iter().enumerate() {
            if device.ms <= best {
                best = device.ms;
                self.gpu.chosen = Some(index);
            }
        }
        if self.mode == Mode::Auto {
            let selected = self
                .gpu
                .chosen
                .map(|i| devices[i].name.as_str())
                .unwrap_or(if self.simd { "SIMD" } else { "CPU" });
            eprintln!("Auto selected {selected}");
        }
    }

    pub fn compute(&mut self, f: &Frame) -> Result<Pair, String> {
        if !f.valid() || f.width * f.height > (i32::MAX as usize) / 3 {
            return Err("Invalid packed BGR frame dimensions".into());
        }
        if self.mode == Mode::Unknown {
            return Err("Unknown mask backend".into());
        }
        let rows = self.ratios.map(|r| {
            let h = f.height as i64;
            let index = (r * f.height as f64).round_ties_even() as i64;
            (if index < 0 { h + index } else { index }).clamp(0, h) as usize
        });
        #[cfg(any(feature = "cuda", feature = "opencl"))]
        let start = {
            if matches!(self.mode, Mode::Auto | Mode::Cuda | Mode::Opencl)
                && (self.gpu.devices.is_none()
                    || (self.mode == Mode::Auto && self.gpu.calls.is_multiple_of(300)))
            {
                self.select(f, rows);
            }
            self.gpu.calls = self.gpu.calls.wrapping_add(1);
            Instant::now()
        };
        let bg = background(f);
        #[cfg(any(feature = "cuda", feature = "opencl"))]
        if let Some(i) = self.gpu.chosen {
            let devices = self.gpu.devices.as_mut().unwrap();
            let d = &mut devices[i];
            match d.compute(f, &bg, rows) {
                Ok(pair) => {
                    self.last = d.kind();
                    if self.mode == Mode::Auto {
                        if self.gpu.samples.len() == 20 {
                            self.gpu.samples.remove(0);
                        }
                        self.gpu.samples.push(start.elapsed().as_secs_f64() * 1000.);
                        if self.gpu.samples.len() >= 10 {
                            let mut sorted = self.gpu.samples.clone();
                            sorted.sort_unstable_by(f64::total_cmp);
                            if sorted[sorted.len() / 2] > (2. * self.gpu.cpu_ms).max(30.) {
                                self.gpu.chosen = None;
                                eprintln!("{} slowed down; switching to CPU", d.name);
                            }
                        }
                    }
                    return Ok(pair);
                }
                Err(e) => {
                    eprintln!("{} failed: {e}; retrying on CPU", d.name);
                    devices.remove(i);
                    self.gpu.chosen = None;
                }
            }
        }
        self.last = if self.simd { "simd" } else { "cpu" };
        Ok(cpu(f, &bg, &self.table, rows, self.simd))
    }
}

#[cfg(any(feature = "cuda", feature = "opencl"))]
fn benchmark(mut run: impl FnMut() -> Result<Pair, String>) -> Result<f64, String> {
    let mut samples = [Duration::ZERO; 5];
    for t in &mut samples {
        let start = Instant::now();
        std::hint::black_box(run()?);
        *t = start.elapsed();
    }
    samples.sort_unstable();
    Ok(samples[2].as_secs_f64() * 1000.)
}

/// Entry point for the driver's disposable preflight process, before game/UI initialization.
pub fn opencl_probe(selector: &str) -> bool {
    #[cfg(feature = "opencl")]
    {
        gpu::probe(selector, &tables(&Config::default()))
    }
    #[cfg(not(feature = "opencl"))]
    {
        let _ = selector;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_and_simd_match_original_masks_at_lane_and_threshold_boundaries() {
        // Reference bytes from the original NumPy/OpenCV CPU backend. The edge
        // bands fix the background; 23 pixels exercise full AVX2 groups and a tail.
        let colors = [
            [80, 80, 80],
            [80, 80, 125],
            [125, 80, 80],
            [80, 125, 80],
            [56, 55, 80],
            [95, 51, 75],
            [120, 50, 70],
            [80, 80, 80],
            [96, 96, 96],
            [95, 95, 95],
            [160, 40, 40],
            [159, 40, 40],
            [110, 88, 160],
            [80, 80, 80],
            [30, 30, 90],
            [1, 0, 1],
            [100, 60, 60],
        ];
        let mut bgr = vec![80; 9];
        bgr.extend(colors.into_iter().flatten());
        bgr.extend([80; 9]);
        let frame = Frame {
            width: 23,
            height: 1,
            bgr,
        };
        let piece = [
            0, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0, 255, 0, 0, 0, 255, 255,
        ];
        let block = [
            0, 255, 255, 255, 255, 255, 255, 0, 255, 0, 255, 255, 255, 0, 255, 255, 255,
        ];
        let cfg = Config::default();
        for backend in ["cpu", "simd"] {
            let (p, b) = Masks::new(&cfg, backend).compute(&frame).unwrap();
            for (mask, expected) in [(p, piece), (b, block)] {
                assert_eq!(&mask[3..20], &expected);
                assert_eq!(&mask[..3], &[0; 3]);
                assert_eq!(&mask[20..], &[0; 3]);
            }
        }
    }
}
