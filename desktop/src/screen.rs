use crate::{Config, Frame, Result};
use image::{
    codecs::png::PngEncoder, DynamicImage, ExtendedColorType, GrayImage, ImageDecoder,
    ImageEncoder, ImageReader,
};
use opencv::{
    core::{self, Mat, Rect, Size, Vec3b},
    imgcodecs, imgproc,
    prelude::*,
};
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::{cell::RefCell, fs, path::Path, sync::Arc};

fn gray(p: &[u8]) -> u8 {
    ((3735 * u32::from(p[0]) + 19235 * u32::from(p[1]) + 9798 * u32::from(p[2]) + 16384) >> 15)
        as u8
}

fn encode_png(pixels: &[u8], w: u32, h: u32, color: ExtendedColorType) -> Result<Vec<u8>> {
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(pixels, w, h, color)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

pub(super) fn glyph_png(glyph: &GrayImage) -> Result<Vec<u8>> {
    encode_png(glyph, glyph.width(), glyph.height(), ExtendedColorType::L8)
}

fn decode_mat(bytes: &[u8], mode: i32) -> Result<Mat> {
    let src = Mat::from_slice(bytes).map_err(|e| e.to_string())?;
    let decoded = imgcodecs::imdecode(&src, mode).map_err(|e| e.to_string())?;
    if decoded.empty() {
        return Err("图像解码失败".into());
    }
    Ok(decoded)
}

impl Frame {
    pub(crate) fn resized_for_recognition(&self, cfg: &Config) -> Result<Option<Self>> {
        let (width, height) = (
            cfg.i("window_ref_width") as usize,
            cfg.i("window_ref_height") as usize,
        );
        let tolerance = cfg.i("window_size_tol_px") as usize;
        if self.width.abs_diff(width) <= tolerance && self.height.abs_diff(height) <= tolerance {
            return Ok(None);
        }
        if !self.valid() || width == 0 || height == 0 {
            return Err("截图像素无效".into());
        }
        let mut bgr = vec![
            0;
            width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(3))
                .ok_or("目标尺寸过大")?
        ];
        let interp = if width <= self.width && height <= self.height {
            imgproc::INTER_AREA
        } else {
            imgproc::INTER_LINEAR
        };
        let src = crate::template::bgr(self).map_err(|e| e.to_string())?;
        let mut dst =
            Mat::new_rows_cols_with_bytes_mut::<Vec3b>(height as i32, width as i32, &mut bgr)
                .map_err(|e| e.to_string())?;
        imgproc::resize(
            &src,
            &mut dst,
            Size::new(width as i32, height as i32),
            0.,
            0.,
            interp,
        )
        .map_err(|e| e.to_string())?;
        Ok(Some(Self { width, height, bgr }))
    }
    pub(crate) fn for_recognition(self, cfg: &Config) -> Result<Self> {
        Ok(self.resized_for_recognition(cfg)?.unwrap_or(self))
    }
    pub fn read(path: &Path) -> Result<Self> {
        let fail = |e: String| format!("{}: {e}", path.display());
        let bytes = fs::read(path).map_err(|e| fail(e.to_string()))?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            let decoded = decode_mat(&bytes, imgcodecs::IMREAD_COLOR).map_err(fail)?;
            return Ok(Self {
                width: decoded.cols() as usize,
                height: decoded.rows() as usize,
                bgr: decoded
                    .data_bytes()
                    .map_err(|e| fail(e.to_string()))?
                    .to_vec(),
            });
        }
        let reader = ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
        let mut decoder = reader.into_decoder().map_err(|e| fail(e.to_string()))?;
        let orientation = decoder.orientation().map_err(|e| fail(e.to_string()))?;
        let mut decoded = DynamicImage::from_decoder(decoder).map_err(|e| fail(e.to_string()))?;
        decoded.apply_orientation(orientation);
        let (width, height) = (decoded.width() as usize, decoded.height() as usize);
        let mut bgr = if decoded.color().bits_per_pixel()
            / u16::from(decoded.color().channel_count())
            == 16
        {
            decoded
                .into_rgb16()
                .into_raw()
                .into_iter()
                .map(|v| (v >> 8) as u8)
                .collect()
        } else {
            decoded.into_rgb8().into_raw()
        };
        for pixel in bgr.chunks_exact_mut(3) {
            pixel.swap(0, 2)
        }
        Ok(Self { width, height, bgr })
    }
    pub fn write(&self, path: &Path) -> Result<()> {
        if !self.valid() {
            return Err("PNG 编码失败".into());
        }
        let mut rgb = self.bgr.clone();
        for p in rgb.chunks_exact_mut(3) {
            p.swap(0, 2)
        }
        fs::write(
            path,
            encode_png(
                &rgb,
                self.width as u32,
                self.height as u32,
                ExtendedColorType::Rgb8,
            )?,
        )
        .map_err(|e| e.to_string())
    }
    pub fn motion_sample(&self, cfg: &Config) -> Result<Vec<u8>> {
        let top = (cfg.f("top_bar_ratio") * self.height as f64).round_ties_even() as isize;
        if !self.valid() || !(0..self.height as isize).contains(&top) {
            return Err("无法计算画面变化".into());
        }
        let sample = || -> opencv::Result<Vec<u8>> {
            let src = crate::template::bgr(self)?;
            let roi = Mat::roi(
                &src,
                Rect::new(0, top as i32, src.cols(), src.rows() - top as i32),
            )?;
            let mut mono = Mat::default();
            imgproc::cvt_color_def(&roi, &mut mono, imgproc::COLOR_BGR2GRAY)?;
            let mut small = Mat::default();
            imgproc::resize(
                &mono,
                &mut small,
                Size::new(96, 176),
                0.,
                0.,
                imgproc::INTER_AREA,
            )?;
            Ok(small.data_bytes()?.to_vec())
        };
        sample().map_err(|e| e.to_string())
    }
    pub fn modal(&self, cfg: &Config) -> bool {
        let mut dark = 0;
        let mut count = 0;
        let threshold = cfg.f("modal_dark_threshold") as i32;
        for y in self.height / 4..self.height * 65 / 100 {
            for x in self.width * 15 / 100..self.width * 85 / 100 {
                let p = &self.bgr[(y * self.width + x) * 3..];
                let gray =
                    (1868 * p[0] as i32 + 9617 * p[1] as i32 + 4899 * p[2] as i32 + 8192) >> 14;
                dark += usize::from(gray < threshold);
                count += 1;
            }
        }
        count > 0 && dark * 100 > 85 * count
    }
    pub fn shift(&self, after: &Frame, min_response: f64) -> Option<(f64, f64)> {
        if !self.valid()
            || !after.valid()
            || (self.width, self.height) != (after.width, after.height)
            || self.width < 2
            || self.height < 2
        {
            return None;
        }
        let grid = Fourier::new(self.width, self.height);
        let hann = |n: usize| {
            (0..n)
                .map(|i| {
                    (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (n - 1) as f64).cos()).sqrt()
                        as f32
                })
                .collect::<Vec<_>>()
        };
        let wx = hann(self.width);
        let wy = hann(self.height);
        let spectrum = |f: &Frame| {
            grid.spectrum(f.width, f.height, |x, y| {
                f32::from(gray(&f.bgr[(y * f.width + x) * 3..])) * wx[x] * wy[y]
            })
        };
        let mut cross = spectrum(self);
        for (a, b) in cross.iter_mut().zip(spectrum(after)) {
            let p = *a * b.conj();
            let mag = p.norm();
            *a = p * (mag / (mag * mag + f32::EPSILON));
        }
        grid.transform(&mut cross, true);
        let (w, h) = (grid.width, grid.height);
        let shifted = |x: usize, y: usize| {
            cross[((y + h.div_ceil(2)) % h) * w + (x + w.div_ceil(2)) % w].re as f64
        };
        let mut peak = (0, 0);
        let mut max = f64::NEG_INFINITY;
        for y in 0..h {
            for x in 0..w {
                let v = shifted(x, y);
                if v > max {
                    max = v;
                    peak = (x, y);
                }
            }
        }
        let (mut sx, mut sy, mut sum) = (0., 0., 0.);
        for y in peak.1.saturating_sub(2)..=(peak.1 + 2).min(h - 1) {
            for x in peak.0.saturating_sub(2)..=(peak.0 + 2).min(w - 1) {
                let v = shifted(x, y);
                sum += v;
                sx += x as f64 * v;
                sy += y as f64 * v;
            }
        }
        (sum / (w * h) as f64 >= min_response).then(|| {
            (
                w as f64 * 0.5 - sx / (sum + f64::EPSILON),
                h as f64 * 0.5 - sy / (sum + f64::EPSILON),
            )
        })
    }
}

struct Fourier {
    width: usize,
    height: usize,
    rows: [Arc<dyn Fft<f32>>; 2],
    columns: [Arc<dyn Fft<f32>>; 2],
}
impl Fourier {
    fn new(w: usize, h: usize) -> Self {
        fn optimal(n: usize) -> usize {
            (n..)
                .find(|&size| {
                    let mut k = size;
                    for p in [2, 3, 5] {
                        while k.is_multiple_of(p) {
                            k /= p
                        }
                    }
                    k == 1
                })
                .unwrap()
        }
        let (width, height) = (optimal(w), optimal(h));
        let mut p = FftPlanner::new();
        Self {
            width,
            height,
            rows: [p.plan_fft_forward(width), p.plan_fft_inverse(width)],
            columns: [p.plan_fft_forward(height), p.plan_fft_inverse(height)],
        }
    }
    fn transform(&self, data: &mut [Complex32], inverse: bool) {
        let row = &self.rows[inverse as usize];
        let col = &self.columns[inverse as usize];
        let mut scratch = vec![
            Complex32::default();
            row.get_inplace_scratch_len()
                .max(col.get_inplace_scratch_len())
        ];
        row.process_with_scratch(data, &mut scratch);
        let mut column = vec![Complex32::default(); self.height];
        for x in 0..self.width {
            for (out, r) in column.iter_mut().zip(data.chunks_exact(self.width)) {
                *out = r[x]
            }
            col.process_with_scratch(&mut column, &mut scratch);
            for (val, r) in column.iter().zip(data.chunks_exact_mut(self.width)) {
                r[x] = *val
            }
        }
    }
    fn spectrum(
        &self,
        w: usize,
        h: usize,
        mut value: impl FnMut(usize, usize) -> f32,
    ) -> Vec<Complex32> {
        let mut data = vec![Complex32::default(); self.width * self.height];
        for (y, row) in data.chunks_exact_mut(self.width).take(h).enumerate() {
            for (x, p) in row.iter_mut().take(w).enumerate() {
                p.re = value(x, y)
            }
        }
        self.transform(&mut data, false);
        data
    }
}

pub(super) fn segment_glyphs(
    frame: &Frame,
    [l, t, r, b]: [usize; 4],
) -> opencv::Result<Vec<GrayImage>> {
    let src = crate::template::bgr(frame)?;
    let roi = Mat::roi(
        &src,
        Rect::new(l as i32, t as i32, (r - l) as i32, (b - t) as i32),
    )?;
    let mut mono = Mat::default();
    imgproc::cvt_color_def(&roi, &mut mono, imgproc::COLOR_BGR2GRAY)?;
    let mut binary = Mat::default();
    imgproc::threshold(
        &mono,
        &mut binary,
        0.,
        255.,
        imgproc::THRESH_BINARY_INV | imgproc::THRESH_OTSU,
    )?;
    let mut closed = Mat::default();
    imgproc::morphology_ex_def(&binary, &mut closed, imgproc::MORPH_CLOSE, &Mat::default())?;
    let mut stats = Mat::default();
    imgproc::connected_components_with_stats(
        &closed,
        &mut Mat::default(),
        &mut stats,
        &mut Mat::default(),
        8,
        core::CV_32S,
    )?;
    let min_height = 6.max((0.35 * (b - t) as f64) as i32);
    let mut boxes: Vec<_> = stats
        .data_typed::<i32>()?
        .chunks_exact(5)
        .skip(1)
        .filter(|s| s[3] >= min_height && s[4] >= 25)
        .map(|s| Rect::new(s[0], s[1], s[2], s[3]))
        .collect();
    boxes.sort_by_key(|s| s.x);
    let stride = r - l;
    let pixels = closed.data_bytes()?;
    Ok(boxes
        .into_iter()
        .map(|rect| {
            GrayImage::from_fn(rect.width as u32, rect.height as u32, |x, y| {
                image::Luma([
                    pixels[(rect.y as usize + y as usize) * stride + rect.x as usize + x as usize]
                ])
            })
        })
        .collect())
}

pub(super) fn glyph_feature(glyph: &GrayImage) -> opencv::Result<Vec<f32>> {
    let (w, h) = (glyph.width(), glyph.height());
    let fitted = (w as f64 * 32. / h as f64).round_ties_even().clamp(1., 24.) as usize;
    let raw = Mat::new_rows_cols_with_data(h as i32, w as i32, glyph.as_raw())?;
    let mut scaled = Mat::default();
    imgproc::resize(
        &raw,
        &mut scaled,
        Size::new(fitted as i32, 32),
        0.,
        0.,
        imgproc::INTER_AREA,
    )?;
    let mut out = vec![0.; 24 * 32];
    for (src, row) in scaled
        .data_bytes()?
        .chunks_exact(fitted)
        .zip(out.chunks_exact_mut(24))
    {
        for (&p, v) in src.iter().zip(&mut row[(24 - fitted) / 2..]) {
            *v = f32::from(p) / 255.
        }
    }
    Ok(out)
}
fn normalized(glyph: &GrayImage) -> opencv::Result<Vec<f32>> {
    let mut v = glyph_feature(glyph)?;
    let length = v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt() + 1e-6;
    for x in &mut v {
        *x = (*x as f64 / length) as f32
    }
    Ok(v)
}

struct ButtonTemplate {
    w: usize,
    h: usize,
    pixels: Vec<u8>,
    mean: f64,
    norm: f64,
}
struct ButtonSpectra {
    size: (usize, usize),
    grid: Fourier,
    spectra: Vec<Vec<Complex32>>,
}
pub struct Screen {
    digits: Vec<Vec<f32>>,
    buttons: Vec<ButtonTemplate>,
    cache: RefCell<Option<ButtonSpectra>>,
}
impl Screen {
    pub fn new(path: &Path, cfg: &Config) -> Result<Self> {
        let threads = cfg.i("cv2_threads");
        let threads = if threads <= 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) as i32
        } else {
            threads.min(128) as i32
        };
        core::set_num_threads(threads).map_err(|e| e.to_string())?;
        let mut digits = Vec::with_capacity(10);
        for id in 0..10 {
            // IMREAD_GRAYSCALE also preserves the codec's grayscale conversion for colored, user-supplied digit templates.
            let path = path.join(format!("digit_{id}.png"));
            let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mono = decode_mat(&bytes, imgcodecs::IMREAD_GRAYSCALE)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let glyph = GrayImage::from_raw(
                mono.cols() as u32,
                mono.rows() as u32,
                mono.data_bytes().map_err(|e| e.to_string())?.to_vec(),
            )
            .ok_or("模板像素尺寸无效")?;
            digits.push(normalized(&glyph).map_err(|e| e.to_string())?);
        }
        let mut buttons = Vec::with_capacity(2);
        for name in ["ui_start.png", "ui_restart.png"] {
            let f = Frame::read(&path.join(name))?;
            let pixels: Vec<_> = f.bgr.chunks_exact(3).map(gray).collect();
            let mean = pixels.iter().map(|p| f64::from(*p)).sum::<f64>() / pixels.len() as f64;
            let norm = pixels
                .iter()
                .map(|p| (f64::from(*p) - mean).powi(2))
                .sum::<f64>()
                .sqrt();
            buttons.push(ButtonTemplate {
                w: f.width,
                h: f.height,
                pixels,
                mean,
                norm,
            });
        }
        Ok(Self {
            digits,
            buttons,
            cache: RefCell::default(),
        })
    }
    pub fn score(&self, f: &Frame, c: &Config) -> Option<i32> {
        if !f.valid() {
            return None;
        }
        let roi = [
            (c.at("score_roi", 0) * f.width as f64) as isize,
            (c.at("score_roi", 1) * f.height as f64) as isize,
            (c.at("score_roi", 2) * f.width as f64) as isize,
            (c.at("score_roi", 3) * f.height as f64) as isize,
        ];
        let [l, t, r, b] = roi;
        if l < 0 || t < 0 || l >= r || t >= b || r > f.width as isize || b > f.height as isize {
            return None;
        }
        let glyphs = segment_glyphs(f, roi.map(|v| v as usize)).ok()?;
        if glyphs.is_empty() {
            return None;
        }
        glyphs.iter().try_fold(0i32, |total, g| {
            let v = normalized(g).ok()?;
            let mut best = (0, f64::NEG_INFINITY);
            for (id, tpl) in self.digits.iter().enumerate() {
                let score = v
                    .iter()
                    .zip(tpl)
                    .map(|(&a, &b)| f64::from(a) * f64::from(b))
                    .sum();
                if score > best.1 {
                    best = (id, score)
                }
            }
            if best.1 < 0.80 {
                return None;
            }
            total.checked_mul(10)?.checked_add(best.0 as i32)
        })
    }
    pub fn button(&self, f: &Frame) -> Result<Option<(bool, [f64; 2])>> {
        if !f.valid() {
            return Err("图像缓冲区尺寸无效".into());
        }
        let (w, h) = (f.width, f.height);
        if self.buttons.iter().all(|t| t.w >= w || t.h >= h) {
            return Ok(None);
        }
        let mut cache = self.cache.borrow_mut();
        if cache.as_ref().is_none_or(|c| c.size != (w, h)) {
            let grid = Fourier::new(w, h);
            let spectra = self
                .buttons
                .iter()
                .map(|t| {
                    if t.w >= w || t.h >= h {
                        return Vec::new();
                    }
                    grid.spectrum(t.w, t.h, |x, y| {
                        (f64::from(t.pixels[y * t.w + x]) - t.mean) as f32
                    })
                })
                .collect();
            *cache = Some(ButtonSpectra {
                size: (w, h),
                grid,
                spectra,
            });
        }
        let c = cache.as_ref().unwrap();
        let pixels: Vec<_> = f.bgr.chunks_exact(3).map(gray).collect();
        let spectrum = c.grid.spectrum(w, h, |x, y| f32::from(pixels[y * w + x]));
        let stride = w + 1;
        let mut integral = vec![[0f64; 2]; stride * (h + 1)];
        for (y, row) in pixels.chunks_exact(w).enumerate() {
            let mut sum = [0.; 2];
            for (x, &p) in row.iter().enumerate() {
                let p = f64::from(p);
                sum[0] += p;
                sum[1] += p * p;
                for k in 0..2 {
                    integral[(y + 1) * stride + x + 1][k] = integral[y * stride + x + 1][k] + sum[k]
                }
            }
        }
        let mut best = None;
        let mut best_score = 0.70;
        for (id, (tpl, kernel)) in self.buttons.iter().zip(&c.spectra).enumerate() {
            if kernel.is_empty() {
                continue;
            }
            let (mut score, mut at) = (f64::NEG_INFINITY, (0, 0));
            if tpl.norm <= f64::EPSILON {
                score = 1.;
            } else {
                let mut corr: Vec<_> = spectrum
                    .iter()
                    .zip(kernel)
                    .map(|(&a, b)| a * b.conj())
                    .collect();
                c.grid.transform(&mut corr, true);
                let scale = 1. / corr.len() as f64;
                let area = (tpl.w * tpl.h) as f64;
                for y in 0..=h - tpl.h {
                    for x in 0..=w - tpl.w {
                        let origin = y * stride + x;
                        let rect = |k: usize| {
                            integral[origin][k] + integral[origin + tpl.h * stride + tpl.w][k]
                                - integral[origin + tpl.w][k]
                                - integral[origin + tpl.h * stride][k]
                        };
                        let variance = rect(1) - rect(0).powi(2) / area;
                        if variance <= 0. {
                            continue;
                        }
                        let sim = ((corr[y * c.grid.width + x].re as f64 * scale)
                            / (tpl.norm * variance.sqrt()))
                        .clamp(-1., 1.);
                        if sim > score {
                            score = sim;
                            at = (x, y)
                        }
                    }
                }
            }
            if score >= best_score {
                best_score = score;
                best = Some((
                    id == 1,
                    [
                        at.0 as f64 + tpl.w as f64 * 0.5,
                        at.1 as f64 + tpl.h as f64 * 0.5,
                    ],
                ))
            }
        }
        Ok(best)
    }
}
