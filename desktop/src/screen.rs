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
use std::{
    any::Any,
    cell::RefCell,
    fs,
    ops::Range,
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
    path::Path,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

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
        for pixel in bgr.as_chunks_mut::<3>().0 {
            pixel.swap(0, 2)
        }
        Ok(Self { width, height, bgr })
    }
    pub fn write(&self, path: &Path) -> Result<()> {
        if !self.valid() {
            return Err("PNG 编码失败".into());
        }
        let mut rgb = self.bgr.clone();
        for p in rgb.as_chunks_mut::<3>().0 {
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
    pub fn has_dark_center(&self, cfg: &Config) -> bool {
        let y_range = self.height / 4..self.height * 65 / 100;
        let x_range = self.width * 15 / 100..self.width * 85 / 100;
        let total = y_range.len() * x_range.len();
        if total == 0 {
            return false;
        }
        let max_bright = (15 * total - 1) / 100;
        let mut bright = 0usize;
        let threshold = cfg.f("modal_dark_threshold") as i32;
        let row_bytes = self.width * 3;
        let x0 = x_range.start * 3;
        let x1 = x_range.end * 3;
        for y in y_range {
            for p in self.bgr[y * row_bytes + x0..y * row_bytes + x1]
                .as_chunks::<3>()
                .0
            {
                let gray =
                    (1868 * p[0] as i32 + 9617 * p[1] as i32 + 4899 * p[2] as i32 + 8192) >> 14;
                bright += usize::from(gray >= threshold);
            }
            if bright > max_bright {
                return false;
            }
        }
        true
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

fn transpose_blocked(src: &[Complex32], dst: &mut [Complex32], rows: usize, cols: usize) {
    const BLOCK: usize = 32;
    assert_eq!(src.len(), rows * cols);
    assert_eq!(dst.len(), rows * cols);
    for r0 in (0..rows).step_by(BLOCK) {
        let r1 = (r0 + BLOCK).min(rows);
        for c0 in (0..cols).step_by(BLOCK) {
            let c1 = (c0 + BLOCK).min(cols);
            for r in r0..r1 {
                let src_row = &src[r * cols..];
                for c in c0..c1 {
                    dst[c * rows + r] = src_row[c];
                }
            }
        }
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
    fn scratch_len(&self) -> usize {
        self.rows
            .iter()
            .chain(self.columns.iter())
            .map(|fft| fft.get_inplace_scratch_len())
            .max()
            .unwrap_or(0)
    }
    fn transform_with_buffers(
        &self,
        data: &mut [Complex32],
        transposed: &mut [Complex32],
        scratch: &mut [Complex32],
        inverse: bool,
    ) {
        let row = &self.rows[inverse as usize];
        let col = &self.columns[inverse as usize];
        row.process_with_scratch(data, scratch);
        transpose_blocked(data, transposed, self.height, self.width);
        col.process_with_scratch(transposed, scratch);
        transpose_blocked(transposed, data, self.width, self.height);
    }
    fn transform(&self, data: &mut [Complex32], inverse: bool) {
        let mut transposed = vec![Complex32::default(); self.width * self.height];
        let mut scratch = vec![Complex32::default(); self.scratch_len()];
        self.transform_with_buffers(data, &mut transposed, &mut scratch, inverse);
    }
    fn transform_forward_transposed(
        &self,
        data: &mut [Complex32],
        out_transposed: &mut [Complex32],
        scratch: &mut [Complex32],
    ) {
        self.rows[0].process_with_scratch(data, scratch);
        transpose_blocked(data, out_transposed, self.height, self.width);
        self.columns[0].process_with_scratch(out_transposed, scratch);
    }
    fn transform_inverse_from_transposed(
        &self,
        transposed: &mut [Complex32],
        out: &mut [Complex32],
        scratch: &mut [Complex32],
    ) {
        self.columns[1].process_with_scratch(transposed, scratch);
        transpose_blocked(transposed, out, self.width, self.height);
        self.rows[1].process_with_scratch(out, scratch);
    }
    fn fill_grid(
        &self,
        w: usize,
        h: usize,
        data: &mut [Complex32],
        mut value: impl FnMut(usize, usize) -> f32,
    ) {
        data.fill(Complex32::default());
        for (y, row) in data.chunks_exact_mut(self.width).take(h).enumerate() {
            for (x, p) in row.iter_mut().take(w).enumerate() {
                p.re = value(x, y);
            }
        }
    }
    fn spectrum(
        &self,
        w: usize,
        h: usize,
        value: impl FnMut(usize, usize) -> f32,
    ) -> Vec<Complex32> {
        let mut data = vec![Complex32::default(); self.width * self.height];
        self.fill_grid(w, h, &mut data, value);
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
        .as_chunks::<5>()
        .0
        .iter()
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
        .zip(out.as_chunks_mut::<24>().0)
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

#[derive(Clone, Copy)]
struct Dispatch {
    func: *const (),
    items: *mut u8,
    count: usize,
    call: unsafe fn(*const (), *mut u8, usize),
}

// SAFETY: `WorkPool::for_each_mut` enforces `F: Sync`, `T: Send`, disjoint per-index access,
// and blocks until every worker finishes before returning or resuming any panic.
unsafe impl Send for Dispatch {}

struct PoolState {
    generation: u64,
    shutdown: bool,
    remaining: usize,
    dispatch: Option<Dispatch>,
    panic: Option<Box<dyn Any + Send>>,
}

type SharedPool = Arc<(Mutex<PoolState>, Condvar, Condvar)>;

/// Persistent thread pool owned by `Screen`'s `ButtonCache` (joined deterministically on Drop).
struct WorkPool {
    shared: SharedPool,
    workers: Vec<JoinHandle<()>>,
}

impl WorkPool {
    fn new(n_workers: usize) -> Self {
        let shared = Arc::new((
            Mutex::new(PoolState {
                generation: 0,
                shutdown: false,
                remaining: 0,
                dispatch: None,
                panic: None,
            }),
            Condvar::new(),
            Condvar::new(),
        ));
        let mut workers = Vec::with_capacity(n_workers);
        for worker_idx in 0..n_workers {
            let shared = Arc::clone(&shared);
            let worker = thread::Builder::new().spawn(move || {
                let (lock, cvar_work, cvar_done) = &*shared;
                let mut seen_gen = 0u64;
                loop {
                    let (idx, dispatch) = {
                        let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                        while !st.shutdown && st.generation == seen_gen {
                            st = cvar_work.wait(st).unwrap_or_else(|e| e.into_inner());
                        }
                        if st.shutdown {
                            return;
                        }
                        seen_gen = st.generation;
                        (worker_idx + 1, st.dispatch)
                    };
                    if let Some(d) = dispatch {
                        if idx < d.count {
                            let res = catch_unwind(AssertUnwindSafe(|| unsafe {
                                (d.call)(d.func, d.items, idx);
                            }));
                            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
                            if let Err(payload) = res {
                                if st.panic.is_none() {
                                    st.panic = Some(payload);
                                }
                            }
                            st.remaining -= 1;
                            if st.remaining == 0 {
                                cvar_done.notify_one();
                            }
                        }
                    }
                }
            });
            match worker {
                Ok(handle) => workers.push(handle),
                Err(_) => break,
            }
        }
        Self { shared, workers }
    }

    fn max_threads(&self) -> usize {
        self.workers.len() + 1
    }

    fn for_each_mut<T: Send, F: Fn(usize, &mut T) + Sync>(&self, items: &mut [T], f: F) {
        if items.len() <= 1 || self.workers.is_empty() {
            for (idx, item) in items.iter_mut().enumerate() {
                f(idx, item);
            }
            return;
        }
        assert!(items.len() <= self.workers.len() + 1);
        unsafe fn invoke<T, F: Fn(usize, &mut T) + Sync>(
            func: *const (),
            items: *mut u8,
            idx: usize,
        ) {
            // SAFETY: `func` points to a live `F: Sync`, `items` points to a disjoint `&mut T` slot.
            let f = unsafe { &*(func as *const F) };
            let item = unsafe { &mut *(items as *mut T).add(idx) };
            f(idx, item);
        }
        let (lock, cvar_work, cvar_done) = &*self.shared;
        {
            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
            assert!(st.dispatch.is_none() && st.remaining == 0);
            st.panic = None;
            st.remaining = items.len() - 1;
            st.dispatch = Some(Dispatch {
                func: (&f as *const F) as *const (),
                items: items.as_mut_ptr() as *mut u8,
                count: items.len(),
                call: invoke::<T, F>,
            });
            st.generation = st.generation.wrapping_add(1);
            cvar_work.notify_all();
        }
        let main_res = catch_unwind(AssertUnwindSafe(|| f(0, &mut items[0])));
        let worker_panic = {
            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
            while st.remaining > 0 {
                st = cvar_done.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            st.dispatch = None;
            st.panic.take()
        };
        if let Err(payload) = main_res {
            resume_unwind(payload);
        }
        if let Some(payload) = worker_panic {
            resume_unwind(payload);
        }
    }
}

impl Drop for WorkPool {
    fn drop(&mut self) {
        let (lock, cvar_work, _) = &*self.shared;
        {
            let mut st = lock.lock().unwrap_or_else(|e| e.into_inner());
            st.shutdown = true;
            cvar_work.notify_all();
        }
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

struct ButtonTemplate {
    w: usize,
    h: usize,
    pixels: Vec<u8>,
    mean: f64,
    norm: f64,
}

impl ButtonTemplate {
    fn is_active_fft(&self, w: usize, h: usize) -> bool {
        self.w < w && self.h < h && self.norm > f64::EPSILON
    }
}

fn band_range(b: usize, n_threads: usize, total: usize) -> Range<usize> {
    let start = b * total / n_threads;
    let end = (b + 1) * total / n_threads;
    start..end
}

#[derive(Clone, Copy)]
struct BandBest {
    score: f64,
    at: (usize, usize),
}

impl Default for BandBest {
    fn default() -> Self {
        Self {
            score: f64::NEG_INFINITY,
            at: (0, 0),
        }
    }
}

struct ScanTemplate<'a> {
    tpl: &'a ButtonTemplate,
    max_y: usize,
    use_imag: bool,
    scale: f64,
    stride: usize,
    w: usize,
    int_sum: &'a [u64],
    int_sum2: &'a [u64],
}

impl<'a> ScanTemplate<'a> {
    fn scan_row(
        &self,
        corr_row: &[Complex32],
        y: usize,
        best: &mut BandBest,
        min_needed: &mut f64,
        min_factor: &mut f64,
    ) {
        let (tw, th) = (self.tpl.w, self.tpl.h);
        let area = (tw * th) as f64;
        let top = &self.int_sum[y * self.stride..];
        let bot = &self.int_sum[(y + th) * self.stride..];
        let top2 = &self.int_sum2[y * self.stride..];
        let bot2 = &self.int_sum2[(y + th) * self.stride..];
        for x in 0..=self.w - tw {
            let c_val =
                f64::from(if self.use_imag { corr_row[x].im } else { corr_row[x].re }) * self.scale;
            if c_val <= 0. {
                continue;
            }
            let s = ((bot[x + tw] - top[x + tw]) - (bot[x] - top[x])) as f64;
            let s2 = ((bot2[x + tw] - top2[x + tw]) - (bot2[x] - top2[x])) as f64;
            let variance = s2 - s * s / area;
            if variance <= 0. || c_val * c_val < *min_factor * variance {
                continue;
            }
            let sim = (c_val / (self.tpl.norm * variance.sqrt())).clamp(-1., 1.);
            if sim > best.score {
                best.score = sim;
                best.at = (x, y);
                if sim > *min_needed {
                    *min_needed = sim;
                    *min_factor = (sim * self.tpl.norm).powi(2) * (1.0 - 1e-12);
                }
            }
        }
    }
}

struct ButtonSpectra {
    size: (usize, usize),
    grid: Fourier,
    half_w: usize,
    active: [bool; 2],
    valid_h: usize,
    combined: Vec<Complex32>,
    row_packed: Vec<Complex32>,
    spec_half: Vec<Complex32>,
    corr_col: Vec<Complex32>,
    int_sum: Vec<u64>,
    int_sum2: Vec<u64>,
    band_sum_off: Vec<u64>,
    band_sum2_off: Vec<u64>,
    thread_scratch: Vec<Vec<Complex32>>,
    thread_tiles: Vec<Vec<Complex32>>,
}

impl ButtonSpectra {
    fn new(buttons: &[ButtonTemplate], w: usize, h: usize, max_threads: usize) -> Self {
        let grid = Fourier::new(w, h);
        let (fw, fh) = (grid.width, grid.height);
        let half_w = fw / 2 + 1;
        let fft_len = fw * fh;
        let active = [
            buttons[0].is_active_fft(w, h),
            buttons[1].is_active_fft(w, h),
        ];
        let valid_h = (0..2)
            .filter(|&i| active[i])
            .map(|i| h - buttons[i].h + 1)
            .max()
            .unwrap_or(0);
        let mut combined = Vec::new();
        if active[0] || active[1] {
            let mut work = vec![Complex32::default(); fft_len];
            let mut scratch = vec![Complex32::default(); grid.scratch_len()];
            let mut specs = [Vec::new(), Vec::new()];
            for i in 0..2 {
                if !active[i] {
                    continue;
                }
                let t = &buttons[i];
                let mut out = vec![Complex32::default(); fft_len];
                grid.fill_grid(t.w, t.h, &mut work, |x, y| {
                    (f64::from(t.pixels[y * t.w + x]) - t.mean) as f32
                });
                grid.transform_forward_transposed(&mut work, &mut out, &mut scratch);
                specs[i] = out;
            }
            combined = vec![Complex32::default(); fft_len];
            for idx in 0..fft_len {
                let a = if active[0] {
                    specs[0][idx].conj()
                } else {
                    Complex32::default()
                };
                let b = if active[1] {
                    specs[1][idx].conj()
                } else {
                    Complex32::default()
                };
                combined[idx] = a + Complex32::new(0., 1.) * b;
            }
        }
        let n_pairs = h.div_ceil(2);
        let stride = w + 1;
        let scratch_len = grid.scratch_len();
        Self {
            size: (w, h),
            grid,
            half_w,
            active,
            valid_h,
            combined,
            row_packed: vec![Complex32::default(); n_pairs * fw],
            spec_half: vec![Complex32::default(); half_w * fh],
            corr_col: vec![Complex32::default(); fft_len],
            int_sum: vec![0; stride * (h + 1)],
            int_sum2: vec![0; stride * (h + 1)],
            band_sum_off: vec![0; max_threads.saturating_sub(1) * stride],
            band_sum2_off: vec![0; max_threads.saturating_sub(1) * stride],
            thread_scratch: (0..max_threads)
                .map(|_| vec![Complex32::default(); scratch_len])
                .collect(),
            thread_tiles: (0..max_threads)
                .map(|_| vec![Complex32::default(); 32 * fw])
                .collect(),
        }
    }

    fn run_fft_ncc(
        &mut self,
        f: &Frame,
        buttons: &[ButtonTemplate],
        pool: Option<&WorkPool>,
    ) -> [BandBest; 2] {
        let (w, h) = self.size;
        let (fw, fh) = (self.grid.width, self.grid.height);
        let half_w = self.half_w;
        let n_pairs = h.div_ceil(2);
        let stride = w + 1;
        let pool_threads = pool.map_or(1, WorkPool::max_threads);
        let n_threads = if w * h >= 65_536 {
            pool_threads
                .min(n_pairs)
                .min(half_w)
                .min(self.valid_h.max(1))
                .max(1)
        } else {
            1
        };

        // Phase 1: Parallel BGR -> gray, 2-rows-in-1 real packing, forward row FFT, and local band integrals.
        self.int_sum[..stride].fill(0);
        self.int_sum2[..stride].fill(0);
        {
            struct Band1<'a> {
                y0: usize,
                y1: usize,
                pairs: usize,
                packed: &'a mut [Complex32],
                sum: &'a mut [u64],
                sum2: &'a mut [u64],
                scratch: &'a mut [Complex32],
            }
            let mut bands = Vec::with_capacity(n_threads);
            let mut rem_packed = self.row_packed.as_mut_slice();
            let mut rem_sum = &mut self.int_sum[stride..];
            let mut rem_sum2 = &mut self.int_sum2[stride..];
            let mut rem_scratch = self.thread_scratch.as_mut_slice();
            for b in 0..n_threads {
                let pr = band_range(b, n_threads, n_pairs);
                let pairs = pr.len();
                let y0 = pr.start * 2;
                let y1 = (pr.end * 2).min(h);
                let (packed, rest_p) = rem_packed.split_at_mut(pairs * fw);
                rem_packed = rest_p;
                let (sum, rest_s) = rem_sum.split_at_mut((y1 - y0) * stride);
                rem_sum = rest_s;
                let (sum2, rest_s2) = rem_sum2.split_at_mut((y1 - y0) * stride);
                rem_sum2 = rest_s2;
                let (scratch_slot, rest_sc) = rem_scratch.split_at_mut(1);
                rem_scratch = rest_sc;
                bands.push(Band1 {
                    y0,
                    y1,
                    pairs,
                    packed,
                    sum,
                    sum2,
                    scratch: &mut scratch_slot[0],
                });
            }
            let row_fft = &self.grid.rows[0];
            let row_bytes = w * 3;
            let bgr = &f.bgr;
            let phase1 = |_, band: &mut Band1<'_>| {
                for lr in 0..band.pairs {
                    let ly = lr * 2;
                    let y_even = band.y0 + ly;
                    let fft_row = &mut band.packed[lr * fw..(lr + 1) * fw];
                    let row0 = &bgr[y_even * row_bytes..(y_even + 1) * row_bytes];
                    let (prev_s, cur_s) = band.sum.split_at_mut(ly * stride);
                    let dst_s = &mut cur_s[..stride];
                    let (prev_s2, cur_s2) = band.sum2.split_at_mut(ly * stride);
                    let dst_s2 = &mut cur_s2[..stride];
                    dst_s[0] = 0;
                    dst_s2[0] = 0;
                    let (mut rs, mut rs2) = (0u64, 0u64);
                    if ly == 0 {
                        for (x, (p, slot)) in row0
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .zip(fft_row[..w].iter_mut())
                            .enumerate()
                        {
                            let g = gray(p);
                            slot.re = f32::from(g);
                            slot.im = 0.;
                            let gu = u64::from(g);
                            rs += gu;
                            rs2 += gu * gu;
                            dst_s[x + 1] = rs;
                            dst_s2[x + 1] = rs2;
                        }
                    } else {
                        let prev_row_s = &prev_s[(ly - 1) * stride..];
                        let prev_row_s2 = &prev_s2[(ly - 1) * stride..];
                        for (x, (p, slot)) in row0
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .zip(fft_row[..w].iter_mut())
                            .enumerate()
                        {
                            let g = gray(p);
                            slot.re = f32::from(g);
                            slot.im = 0.;
                            let gu = u64::from(g);
                            rs += gu;
                            rs2 += gu * gu;
                            dst_s[x + 1] = prev_row_s[x + 1] + rs;
                            dst_s2[x + 1] = prev_row_s2[x + 1] + rs2;
                        }
                    }
                    if y_even + 1 < band.y1 {
                        let row1 = &bgr[(y_even + 1) * row_bytes..(y_even + 2) * row_bytes];
                        let ly1 = ly + 1;
                        let (prev_s, cur_s) = band.sum.split_at_mut(ly1 * stride);
                        let prev_row_s = &prev_s[ly * stride..];
                        let dst_s = &mut cur_s[..stride];
                        let (prev_s2, cur_s2) = band.sum2.split_at_mut(ly1 * stride);
                        let prev_row_s2 = &prev_s2[ly * stride..];
                        let dst_s2 = &mut cur_s2[..stride];
                        dst_s[0] = 0;
                        dst_s2[0] = 0;
                        let (mut rs, mut rs2) = (0u64, 0u64);
                        for (x, (p, slot)) in row1
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .zip(fft_row[..w].iter_mut())
                            .enumerate()
                        {
                            let g = gray(p);
                            slot.im = f32::from(g);
                            let gu = u64::from(g);
                            rs += gu;
                            rs2 += gu * gu;
                            dst_s[x + 1] = prev_row_s[x + 1] + rs;
                            dst_s2[x + 1] = prev_row_s2[x + 1] + rs2;
                        }
                    }
                    fft_row[w..].fill(Complex32::default());
                    row_fft.process_with_scratch(fft_row, band.scratch);
                }
            };
            if n_threads > 1 {
                if let Some(p) = pool {
                    p.for_each_mut(&mut bands, phase1);
                } else {
                    phase1(0, &mut bands[0]);
                }
            } else {
                phase1(0, &mut bands[0]);
            }
        }

        // Phase 1B: Fold vertical band totals into integral rows for bands 1..n_threads.
        if n_threads > 1 {
            let y1_0 = (band_range(0, n_threads, n_pairs).end * 2).min(h);
            self.band_sum_off[..stride].copy_from_slice(&self.int_sum[y1_0 * stride..(y1_0 + 1) * stride]);
            self.band_sum2_off[..stride]
                .copy_from_slice(&self.int_sum2[y1_0 * stride..(y1_0 + 1) * stride]);
            for b in 2..n_threads {
                let prev_end_y = (band_range(b - 1, n_threads, n_pairs).end * 2).min(h);
                let prev_row_s = &self.int_sum[prev_end_y * stride..(prev_end_y + 1) * stride];
                let prev_row_s2 = &self.int_sum2[prev_end_y * stride..(prev_end_y + 1) * stride];
                let (prev_off_s, cur_off_s) = self.band_sum_off.split_at_mut((b - 1) * stride);
                let prev_off_s = &prev_off_s[(b - 2) * stride..];
                let (prev_off_s2, cur_off_s2) = self.band_sum2_off.split_at_mut((b - 1) * stride);
                let prev_off_s2 = &prev_off_s2[(b - 2) * stride..];
                for x in 1..stride {
                    cur_off_s[x] = prev_off_s[x] + prev_row_s[x];
                    cur_off_s2[x] = prev_off_s2[x] + prev_row_s2[x];
                }
            }
            struct Band1B<'a> {
                sum: &'a mut [u64],
                sum2: &'a mut [u64],
                off_s: &'a [u64],
                off_s2: &'a [u64],
            }
            let mut bands = Vec::with_capacity(n_threads - 1);
            let mut rem_sum = &mut self.int_sum[(y1_0 + 1) * stride..];
            let mut rem_sum2 = &mut self.int_sum2[(y1_0 + 1) * stride..];
            for b in 1..n_threads {
                let pr = band_range(b, n_threads, n_pairs);
                let rows = (pr.end * 2).min(h) - pr.start * 2;
                let (sum, rest_s) = rem_sum.split_at_mut(rows * stride);
                rem_sum = rest_s;
                let (sum2, rest_s2) = rem_sum2.split_at_mut(rows * stride);
                rem_sum2 = rest_s2;
                let off_s = &self.band_sum_off[(b - 1) * stride..b * stride];
                let off_s2 = &self.band_sum2_off[(b - 1) * stride..b * stride];
                bands.push(Band1B {
                    sum,
                    sum2,
                    off_s,
                    off_s2,
                });
            }
            let phase1b = |_, band: &mut Band1B<'_>| {
                for row in band.sum.chunks_exact_mut(stride) {
                    for (dst, &off) in row[1..].iter_mut().zip(&band.off_s[1..]) {
                        *dst += off;
                    }
                }
                for row in band.sum2.chunks_exact_mut(stride) {
                    for (dst, &off) in row[1..].iter_mut().zip(&band.off_s2[1..]) {
                        *dst += off;
                    }
                }
            };
            if let Some(p) = pool {
                p.for_each_mut(&mut bands, phase1b);
            } else {
                for (i, band) in bands.iter_mut().enumerate() {
                    phase1b(i, band);
                }
            }
        }

        // Phase 2: Blocked unpack-transpose for columns 0..half_w + forward column FFT.
        {
            struct Band2<'a> {
                c0: usize,
                c1: usize,
                spec: &'a mut [Complex32],
                scratch: &'a mut [Complex32],
            }
            let mut bands = Vec::with_capacity(n_threads);
            let mut rem_spec = self.spec_half.as_mut_slice();
            let mut rem_scratch = self.thread_scratch.as_mut_slice();
            for b in 0..n_threads {
                let cr = band_range(b, n_threads, half_w);
                let (spec, rest_sp) = rem_spec.split_at_mut(cr.len() * fh);
                rem_spec = rest_sp;
                let (scratch_slot, rest_sc) = rem_scratch.split_at_mut(1);
                rem_scratch = rest_sc;
                bands.push(Band2 {
                    c0: cr.start,
                    c1: cr.end,
                    spec,
                    scratch: &mut scratch_slot[0],
                });
            }
            let row_packed = &self.row_packed;
            let col_fft = &self.grid.columns[0];
            let phase2 = |_, band: &mut Band2<'_>| {
                for r0 in (0..n_pairs).step_by(16) {
                    let r1 = (r0 + 16).min(n_pairs);
                    for k0 in (band.c0..band.c1).step_by(32) {
                        let k1 = (k0 + 32).min(band.c1);
                        for k in k0..k1 {
                            let k_neg = if k == 0 { 0 } else { fw - k };
                            let col = &mut band.spec[(k - band.c0) * fh..(k - band.c0 + 1) * fh];
                            for r in r0..r1 {
                                let a = row_packed[r * fw + k];
                                let b = row_packed[r * fw + k_neg];
                                col[2 * r] =
                                    Complex32::new(0.5 * (a.re + b.re), 0.5 * (a.im - b.im));
                                if 2 * r + 1 < h {
                                    col[2 * r + 1] =
                                        Complex32::new(0.5 * (a.im + b.im), 0.5 * (b.re - a.re));
                                }
                            }
                        }
                    }
                }
                for col in band.spec.chunks_exact_mut(fh) {
                    col[h..fh].fill(Complex32::default());
                }
                col_fft.process_with_scratch(band.spec, band.scratch);
            };
            if n_threads > 1 {
                if let Some(p) = pool {
                    p.for_each_mut(&mut bands, phase2);
                } else {
                    phase2(0, &mut bands[0]);
                }
            } else {
                phase2(0, &mut bands[0]);
            }
        }

        // Phase 3: Conjugate-mirrored multiply against T0* + i*T1* and inverse column FFT.
        {
            struct Band3<'a> {
                c0: usize,
                c1: usize,
                corr: &'a mut [Complex32],
                scratch: &'a mut [Complex32],
            }
            let mut bands = Vec::with_capacity(n_threads);
            let mut rem_corr = self.corr_col.as_mut_slice();
            let mut rem_scratch = self.thread_scratch.as_mut_slice();
            for b in 0..n_threads {
                let cr = band_range(b, n_threads, fw);
                let (corr, rest_c) = rem_corr.split_at_mut(cr.len() * fh);
                rem_corr = rest_c;
                let (scratch_slot, rest_sc) = rem_scratch.split_at_mut(1);
                rem_scratch = rest_sc;
                bands.push(Band3 {
                    c0: cr.start,
                    c1: cr.end,
                    corr,
                    scratch: &mut scratch_slot[0],
                });
            }
            let spec_half = &self.spec_half;
            let combined = &self.combined;
            let col_fft_inv = &self.grid.columns[1];
            let phase3 = |_, band: &mut Band3<'_>| {
                for k in band.c0..band.c1 {
                    let dst = &mut band.corr[(k - band.c0) * fh..(k - band.c0 + 1) * fh];
                    let comb = &combined[k * fh..(k + 1) * fh];
                    if k < half_w {
                        let src = &spec_half[k * fh..(k + 1) * fh];
                        for ((d, &s), &c) in dst.iter_mut().zip(src).zip(comb) {
                            *d = s * c;
                        }
                    } else {
                        let mk = fw - k;
                        let src = &spec_half[mk * fh..(mk + 1) * fh];
                        dst[0] = src[0].conj() * comb[0];
                        for ky in 1..fh {
                            dst[ky] = src[fh - ky].conj() * comb[ky];
                        }
                    }
                }
                col_fft_inv.process_with_scratch(band.corr, band.scratch);
            };
            if n_threads > 1 {
                if let Some(p) = pool {
                    p.for_each_mut(&mut bands, phase3);
                } else {
                    phase3(0, &mut bands[0]);
                }
            } else {
                phase3(0, &mut bands[0]);
            }
        }

        // Phase 4: 32-row L2-tiled transpose, inverse row FFT, and fused NCC search across active templates.
        let mut acc = [BandBest::default(); 2];
        {
            struct Band4<'a> {
                y0: usize,
                y1: usize,
                tile: &'a mut [Complex32],
                scratch: &'a mut [Complex32],
                best: [BandBest; 2],
            }
            let mut bands = Vec::with_capacity(n_threads);
            let mut rem_tiles = self.thread_tiles.as_mut_slice();
            let mut rem_scratch = self.thread_scratch.as_mut_slice();
            for b in 0..n_threads {
                let yr = band_range(b, n_threads, self.valid_h);
                let (tile_slot, rest_t) = rem_tiles.split_at_mut(1);
                rem_tiles = rest_t;
                let (scratch_slot, rest_sc) = rem_scratch.split_at_mut(1);
                rem_scratch = rest_sc;
                bands.push(Band4 {
                    y0: yr.start,
                    y1: yr.end,
                    tile: &mut tile_slot[0],
                    scratch: &mut scratch_slot[0],
                    best: [BandBest::default(); 2],
                });
            }
            let scale = 1. / (fw * fh) as f64;
            let scans = [
                self.active[0].then(|| ScanTemplate {
                    tpl: &buttons[0],
                    max_y: h - buttons[0].h,
                    use_imag: false,
                    scale,
                    stride,
                    w,
                    int_sum: &self.int_sum,
                    int_sum2: &self.int_sum2,
                }),
                self.active[1].then(|| ScanTemplate {
                    tpl: &buttons[1],
                    max_y: h - buttons[1].h,
                    use_imag: true,
                    scale,
                    stride,
                    w,
                    int_sum: &self.int_sum,
                    int_sum2: &self.int_sum2,
                }),
            ];
            let corr_col = &self.corr_col;
            let row_fft_inv = &self.grid.rows[1];
            let phase4 = |_, band: &mut Band4<'_>| {
                let mut min_needed = [0.70f64; 2];
                let mut min_factor = [
                    scans[0]
                        .as_ref()
                        .map_or(0., |s| (0.70 * s.tpl.norm).powi(2) * (1.0 - 1e-12)),
                    scans[1]
                        .as_ref()
                        .map_or(0., |s| (0.70 * s.tpl.norm).powi(2) * (1.0 - 1e-12)),
                ];
                for ry0 in (band.y0..band.y1).step_by(32) {
                    let ry1 = (ry0 + 32).min(band.y1);
                    let strip_h = ry1 - ry0;
                    let tile = &mut band.tile[..strip_h * fw];
                    for k0 in (0..fw).step_by(32) {
                        let k1 = (k0 + 32).min(fw);
                        for k in k0..k1 {
                            let col = &corr_col[k * fh + ry0..k * fh + ry1];
                            for (dy, &val) in col.iter().enumerate() {
                                tile[dy * fw + k] = val;
                            }
                        }
                    }
                    row_fft_inv.process_with_scratch(tile, band.scratch);
                    for dy in 0..strip_h {
                        let y = ry0 + dy;
                        let corr_row = &tile[dy * fw..dy * fw + w];
                        for (id, scan) in scans.iter().enumerate() {
                            if let Some(s) = scan {
                                if y <= s.max_y {
                                    s.scan_row(
                                        corr_row,
                                        y,
                                        &mut band.best[id],
                                        &mut min_needed[id],
                                        &mut min_factor[id],
                                    );
                                }
                            }
                        }
                    }
                }
            };
            if n_threads > 1 {
                if let Some(p) = pool {
                    p.for_each_mut(&mut bands, phase4);
                } else {
                    phase4(0, &mut bands[0]);
                }
            } else {
                phase4(0, &mut bands[0]);
            }
            for band in bands {
                for id in 0..2 {
                    if band.best[id].score > acc[id].score {
                        acc[id] = band.best[id];
                    }
                }
            }
        }
        acc
    }
}

#[derive(Default)]
struct ButtonCache {
    pool: Option<WorkPool>,
    spectra: Option<ButtonSpectra>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ScreenState {
    Board,
    Start([f64; 2]),
    Restart([f64; 2]),
    ResultPending,
    SkinModal,
    UnknownDark,
}

impl ScreenState {
    pub(crate) fn is_result(&self) -> bool {
        matches!(self, Self::Restart(_) | Self::ResultPending)
    }

    pub(crate) fn playable(
        &self,
        f: &Frame,
        cfg: &Config,
        screen: &Screen,
        d: &crate::vision::Detection,
    ) -> bool {
        match self {
            Self::Board => true,
            Self::UnknownDark => {
                screen.score(f, cfg).is_some() && crate::vision::clear_dark_board(f, d, cfg)
            }
            _ => false,
        }
    }
}

struct ScreenTextScratch {
    resized: Mat,
    buf_bg: Vec<Complex32>,
    buf_r: Vec<Complex32>,
    spec_bg: Vec<Complex32>,
    spec_r: Vec<Complex32>,
    corr: Vec<Complex32>,
    fft_scratch: Vec<Complex32>,
    int_sum: Vec<[u32; 3]>,
    int_sum2: Vec<u64>,
}

/// Text and search bounds are calibrated in the original 553 x 1050 client.
struct ScreenText {
    #[cfg(test)]
    text: Mat,
    area: Rect,
    tw: usize,
    th: usize,
    t_norm: f64,
    grid: Fourier,
    tpl_conj: [Vec<Complex32>; 3],
    scratch: RefCell<ScreenTextScratch>,
}

impl ScreenText {
    fn new(png: &[u8], area: Rect) -> Result<Self> {
        let text = decode_mat(png, imgcodecs::IMREAD_COLOR).map_err(|e| e.to_string())?;
        let (tw, th) = (text.cols() as usize, text.rows() as usize);
        let (rw, rh) = (area.width as usize, area.height as usize);
        if tw == 0 || th == 0 || tw > rw || th > rh {
            return Err("UI 文本模板尺寸无效".into());
        }
        let bytes = text.data_bytes().map_err(|e| e.to_string())?;
        let n = (tw * th) as f64;
        let mut sums = [0f64; 3];
        for p in bytes.as_chunks::<3>().0 {
            sums[0] += f64::from(p[0]);
            sums[1] += f64::from(p[1]);
            sums[2] += f64::from(p[2]);
        }
        let mean = [sums[0] / n, sums[1] / n, sums[2] / n];
        let mut norm_sq = 0.;
        for p in bytes.as_chunks::<3>().0 {
            for c in 0..3 {
                let d = f64::from(p[c]) - mean[c];
                norm_sq += d * d;
            }
        }
        let t_norm = norm_sq.sqrt();
        let grid = Fourier::new(rw, rh);
        let fft_len = grid.width * grid.height;
        let mut work = vec![Complex32::default(); fft_len];
        let mut fft_scratch = vec![Complex32::default(); grid.scratch_len()];
        let mut tpl_conj = [Vec::new(), Vec::new(), Vec::new()];
        for c in 0..3 {
            grid.fill_grid(tw, th, &mut work, |x, y| {
                (f64::from(bytes[(y * tw + x) * 3 + c]) - mean[c]) as f32
            });
            let mut spec = vec![Complex32::default(); fft_len];
            grid.transform_forward_transposed(&mut work, &mut spec, &mut fft_scratch);
            for z in &mut spec {
                *z = z.conj();
            }
            tpl_conj[c] = spec;
        }
        let int_len = (rw + 1) * (rh + 1);
        Ok(Self {
            #[cfg(test)]
            text,
            area,
            tw,
            th,
            t_norm,
            grid,
            tpl_conj,
            scratch: RefCell::new(ScreenTextScratch {
                resized: Mat::default(),
                buf_bg: vec![Complex32::default(); fft_len],
                buf_r: vec![Complex32::default(); fft_len],
                spec_bg: vec![Complex32::default(); fft_len],
                spec_r: vec![Complex32::default(); fft_len],
                corr: vec![Complex32::default(); fft_len],
                fft_scratch,
                int_sum: vec![[0; 3]; int_len],
                int_sum2: vec![0; int_len],
            }),
        })
    }

    fn center(&self, f: &Frame) -> opencv::Result<Option<[f64; 2]>> {
        let a = self.area;
        let x = |n: i32| (n as f64 * f.width as f64 / 553.).round() as i32;
        let y = |n: i32| (n as f64 * f.height as f64 / 1050.).round() as i32;
        let bounds = Rect::new(
            x(a.x),
            y(a.y),
            x(a.x + a.width) - x(a.x),
            y(a.y + a.height) - y(a.y),
        );
        if bounds.empty() {
            return Ok(None);
        }
        if bounds.x < 0
            || bounds.y < 0
            || bounds.x + bounds.width > f.width as i32
            || bounds.y + bounds.height > f.height as i32
        {
            let image = crate::template::bgr(f)?;
            let _ = Mat::roi(&image, bounds)?;
            return Ok(None);
        }
        let (x0, y0, bx_w, bx_h) = (
            bounds.x as usize,
            bounds.y as usize,
            bounds.width as usize,
            bounds.height as usize,
        );
        let f_stride = f.width * 3;
        let first = &f.bgr[y0 * f_stride + x0 * 3..][..3];
        let all_equal = (y0..y0 + bx_h).all(|ry| {
            f.bgr[ry * f_stride + x0 * 3..ry * f_stride + (x0 + bx_w) * 3]
                .as_chunks::<3>()
                .0
                .iter()
                .all(|p| p == first)
        });
        if all_equal {
            return Ok(None);
        }
        if self.t_norm <= f64::EPSILON {
            return Ok(Some([
                bounds.x as f64 + (self.tw as f64 / 2.) * bounds.width as f64 / a.width as f64,
                bounds.y as f64 + (self.th as f64 / 2.) * bounds.height as f64 / a.height as f64,
            ]));
        }

        let ScreenTextScratch {
            resized,
            buf_bg,
            buf_r,
            spec_bg,
            spec_r,
            corr,
            fft_scratch,
            int_sum,
            int_sum2,
        } = &mut *self.scratch.borrow_mut();

        let (rw, rh) = (a.width as usize, a.height as usize);
        let (fw, fh) = (self.grid.width, self.grid.height);
        let stride = rw + 1;
        let (src_slice, src_stride, src_base): (&[u8], usize, usize) =
            if bounds.width == a.width && bounds.height == a.height {
                (&f.bgr, f_stride, y0 * f_stride + x0 * 3)
            } else {
                let image = crate::template::bgr(f)?;
                let roi = Mat::roi(&image, bounds)?;
                imgproc::resize(&roi, resized, a.size(), 0., 0., imgproc::INTER_LINEAR)?;
                (resized.data_bytes()?, rw * 3, 0)
            };

        buf_bg.fill(Complex32::default());
        buf_r.fill(Complex32::default());
        int_sum[..stride].fill([0; 3]);
        int_sum2[..stride].fill(0);
        for ry in 0..rh {
            let row = &src_slice[src_base + ry * src_stride..][..rw * 3];
            let bg_row = &mut buf_bg[ry * fw..ry * fw + rw];
            let r_row = &mut buf_r[ry * fw..ry * fw + rw];
            let (prev_s, cur_s) = int_sum.split_at_mut((ry + 1) * stride);
            let prev_row_s = &prev_s[ry * stride..];
            let dst_s = &mut cur_s[..stride];
            let (prev_s2, cur_s2) = int_sum2.split_at_mut((ry + 1) * stride);
            let prev_row_s2 = &prev_s2[ry * stride..];
            let dst_s2 = &mut cur_s2[..stride];
            dst_s[0] = [0; 3];
            dst_s2[0] = 0;
            let (mut rs0, mut rs1, mut rs2, mut rs_sq) = (0u32, 0u32, 0u32, 0u64);
            for (rx, p) in row.as_chunks::<3>().0.iter().enumerate() {
                let (b, g, r) = (p[0], p[1], p[2]);
                bg_row[rx] = Complex32::new(f32::from(b), f32::from(g));
                r_row[rx] = Complex32::new(f32::from(r), 0.);
                rs0 += u32::from(b);
                rs1 += u32::from(g);
                rs2 += u32::from(r);
                rs_sq += u64::from(b) * u64::from(b)
                    + u64::from(g) * u64::from(g)
                    + u64::from(r) * u64::from(r);
                let ps = prev_row_s[rx + 1];
                dst_s[rx + 1] = [ps[0] + rs0, ps[1] + rs1, ps[2] + rs2];
                dst_s2[rx + 1] = prev_row_s2[rx + 1] + rs_sq;
            }
        }

        self.grid
            .transform_forward_transposed(buf_bg, spec_bg, fft_scratch);
        self.grid
            .transform_forward_transposed(buf_r, spec_r, fft_scratch);
        let [t0, t1, t2] = &self.tpl_conj;
        for k in 0..fw {
            let k_neg = if k == 0 { 0 } else { fw - k };
            let u_col = &spec_bg[k * fh..(k + 1) * fh];
            let v_col = &spec_bg[k_neg * fh..(k_neg + 1) * fh];
            let r_col = &spec_r[k * fh..(k + 1) * fh];
            let d_col = &mut corr[k * fh..(k + 1) * fh];
            let col_off = k * fh;
            {
                let (u, v) = (u_col[0], v_col[0]);
                let b_hat = Complex32::new(0.5 * (u.re + v.re), 0.5 * (u.im - v.im));
                let g_hat = Complex32::new(0.5 * (u.im + v.im), 0.5 * (v.re - u.re));
                d_col[0] = b_hat * t0[col_off] + g_hat * t1[col_off] + r_col[0] * t2[col_off];
            }
            for ky in 1..fh {
                let (u, v) = (u_col[ky], v_col[fh - ky]);
                let b_hat = Complex32::new(0.5 * (u.re + v.re), 0.5 * (u.im - v.im));
                let g_hat = Complex32::new(0.5 * (u.im + v.im), 0.5 * (v.re - u.re));
                let idx = col_off + ky;
                d_col[ky] = b_hat * t0[idx] + g_hat * t1[idx] + r_col[ky] * t2[idx];
            }
        }
        self.grid
            .transform_inverse_from_transposed(corr, buf_bg, fft_scratch);

        let (tw, th) = (self.tw, self.th);
        let area_f = (tw * th) as f64;
        let scale = 1. / (fw * fh) as f64;
        let mut peak = f64::NEG_INFINITY;
        let mut at = (0usize, 0usize);
        let mut min_factor = (0.90 * self.t_norm).powi(2) * (1.0 - 1e-12);
        for ry in 0..=rh - th {
            let top = &int_sum[ry * stride..];
            let bot = &int_sum[(ry + th) * stride..];
            let top2 = &int_sum2[ry * stride..];
            let bot2 = &int_sum2[(ry + th) * stride..];
            let corr_row = &buf_bg[ry * fw..];
            for rx in 0..=rw - tw {
                let c_val = f64::from(corr_row[rx].re) * scale;
                if c_val <= 0. {
                    continue;
                }
                let [b0, g0, r0] = top[rx];
                let [b1, g1, r1] = top[rx + tw];
                let [b2, g2, r2] = bot[rx];
                let [b3, g3, r3] = bot[rx + tw];
                let sb = ((b3 - b1) - (b2 - b0)) as f64;
                let sg = ((g3 - g1) - (g2 - g0)) as f64;
                let sr = ((r3 - r1) - (r2 - r0)) as f64;
                let sum2 = ((bot2[rx + tw] - top2[rx + tw]) - (bot2[rx] - top2[rx])) as f64;
                let variance = sum2 - (sb * sb + sg * sg + sr * sr) / area_f;
                if variance <= 1e-6 || c_val * c_val < min_factor * variance {
                    continue;
                }
                let sim = (c_val / (self.t_norm * variance.sqrt())).clamp(-1., 1.);
                if sim > peak {
                    peak = sim;
                    at = (rx, ry);
                    if sim > 0.90 {
                        min_factor = (sim * self.t_norm).powi(2) * (1.0 - 1e-12);
                    }
                }
            }
        }
        Ok((peak >= 0.90).then(|| {
            [
                bounds.x as f64 + (at.0 as f64 + tw as f64 / 2.) * bounds.width as f64 / a.width as f64,
                bounds.y as f64 + (at.1 as f64 + th as f64 / 2.) * bounds.height as f64 / a.height as f64,
            ]
        }))
    }
}

pub struct Screen {
    digits: Vec<Vec<f32>>,
    buttons: Vec<ButtonTemplate>,
    skin_title: ScreenText,
    play_again: ScreenText,
    new_record: ScreenText,
    fft_threads: usize,
    cache: RefCell<ButtonCache>,
}
impl Screen {
    pub fn new(path: &Path, cfg: &Config) -> Result<Self> {
        let threads_cfg = cfg.i("cv2_threads");
        let threads = if threads_cfg <= 0 {
            thread::available_parallelism().map_or(1, |n| n.get().min(4)) as i32
        } else {
            threads_cfg.min(128) as i32
        };
        core::set_num_threads(threads).map_err(|e| e.to_string())?;
        let fft_threads = if threads_cfg <= 0 {
            thread::available_parallelism().map_or(1, |n| n.get().min(6))
        } else {
            (threads_cfg as usize).clamp(1, 8)
        };
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
            let pixels: Vec<_> = f.bgr.as_chunks::<3>().0.iter().map(|p| gray(p)).collect();
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
            skin_title: ScreenText::new(
                include_bytes!("../ui/skin_title.png"),
                Rect::new(190, 160, 173, 105),
            )?,
            play_again: ScreenText::new(
                include_bytes!("../ui/play_again.png"),
                Rect::new(175, 815, 203, 129),
            )?,
            new_record: ScreenText::new(
                include_bytes!("../ui/new_record.png"),
                Rect::new(195, 147, 163, 130),
            )?,
            fft_threads,
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
    /// Input and returned click coordinates use the same recognition frame (after for_recognition).
    pub(crate) fn state(&self, f: &Frame, cfg: &Config) -> Result<ScreenState> {
        if !f.valid() {
            return Err("图像缓冲区尺寸无效".into());
        }
        let center = |text: &ScreenText| text.center(f).map_err(|e| e.to_string());
        if center(&self.skin_title)?.is_some() {
            return Ok(ScreenState::SkinModal);
        }
        if let Some((restart, at)) = self.button(f)? {
            return Ok(if restart {
                ScreenState::Restart(at)
            } else {
                ScreenState::Start(at)
            });
        }
        let dark = f.has_dark_center(cfg);
        if dark {
            if let Some(at) = center(&self.play_again)? {
                return Ok(ScreenState::Restart(at));
            }
        }
        if center(&self.new_record)?.is_some() {
            return Ok(ScreenState::ResultPending);
        }
        Ok(if dark {
            ScreenState::UnknownDark
        } else {
            ScreenState::Board
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
        let cache = &mut *self.cache.borrow_mut();
        let use_pool = w * h >= 65_536 && self.fft_threads > 1;
        if use_pool && cache.pool.is_none() {
            cache.pool = Some(WorkPool::new(self.fft_threads - 1));
        }
        let pool_threads = cache.pool.as_ref().map_or(1, WorkPool::max_threads);
        if cache
            .spectra
            .as_ref()
            .is_none_or(|s| s.size != (w, h) || s.thread_scratch.len() < pool_threads)
        {
            cache.spectra = Some(ButtonSpectra::new(&self.buttons, w, h, pool_threads));
        }
        let spectra = cache.spectra.as_mut().unwrap();
        let band_best = (spectra.active[0] || spectra.active[1])
            .then(|| spectra.run_fft_ncc(f, &self.buttons, cache.pool.as_ref()));
        let mut best = None;
        let mut best_score = 0.70;
        for (id, tpl) in self.buttons.iter().enumerate() {
            if tpl.w >= w || tpl.h >= h {
                continue;
            }
            let (score, at) = if tpl.norm <= f64::EPSILON {
                (1., (0, 0))
            } else if let Some(acc) = band_best {
                (acc[id].score, acc[id].at)
            } else {
                continue;
            };
            if score >= best_score {
                best_score = score;
                best = Some((
                    id == 1,
                    [
                        at.0 as f64 + tpl.w as f64 * 0.5,
                        at.1 as f64 + tpl.h as f64 * 0.5,
                    ],
                ));
            }
        }
        Ok(best)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pasted_buttons_return_their_center_after_frame_dimensions_change() {
        let dir = crate::asset_dir().join("templates");
        let screen = Screen::new(&dir, &Config::default()).unwrap();
        for (file, restart) in [("ui_start.png", false), ("ui_restart.png", true)] {
            let t = Frame::read(&dir.join(file)).unwrap();
            let (width, height) = (t.width + 51, t.height + 49);
            let mut frame = Frame {
                width,
                height,
                bgr: vec![127; width * height * 3],
            };
            assert_eq!(screen.button(&frame).unwrap(), None);
            for (y, row) in t.bgr.chunks_exact(t.width * 3).enumerate() {
                let offset = ((13 + y) * width + 7) * 3;
                frame.bgr[offset..offset + row.len()].copy_from_slice(row);
            }
            assert_eq!(
                screen.button(&frame).unwrap(),
                Some((
                    restart,
                    [7. + t.width as f64 / 2., 13. + t.height as f64 / 2.]
                ))
            );
        }
    }
    #[test]
    fn new_restart_requires_a_dark_frame_and_skin_title_takes_priority() {
        let cfg = Config::default();
        let screen = Screen::new(&crate::asset_dir().join("templates"), &cfg).unwrap();
        let mut f = Frame {
            width: 553,
            height: 1050,
            bgr: vec![15; 553 * 1050 * 3],
        };
        fn paste(f: &mut Frame, data: &Mat, x: usize, y: usize) {
            for (dy, row) in data
                .data_bytes()
                .unwrap()
                .chunks_exact(data.cols() as usize * 3)
                .enumerate()
            {
                let i = ((y + dy) * f.width + x) * 3;
                f.bgr[i..i + row.len()].copy_from_slice(row);
            }
        }
        let d = crate::vision::Detection {
            piece: None,
            target: None,
            piece_mask: vec![],
            block_mask: vec![],
        };
        let unknown = screen.state(&f, &cfg).unwrap();
        assert_eq!(unknown, ScreenState::UnknownDark);
        assert!(!unknown.playable(&f, &cfg, &screen, &d));
        paste(&mut f, &screen.play_again.text, 217, 500);
        assert_eq!(screen.state(&f, &cfg).unwrap(), ScreenState::UnknownDark);
        paste(&mut f, &screen.new_record.text, 247, 191);
        let pending = screen.state(&f, &cfg).unwrap();
        assert_eq!(pending, ScreenState::ResultPending);
        assert!(pending.is_result());
        assert!(!pending.playable(&f, &cfg, &screen, &d));
        paste(&mut f, &screen.play_again.text, 217, 856);
        let restart = screen.state(&f, &cfg).unwrap();
        assert_eq!(restart, ScreenState::Restart([277., 872.5]));
        assert!(restart.is_result());
        paste(&mut f, &screen.skin_title.text, 219, 204);
        let skin = screen.state(&f, &cfg).unwrap();
        assert_eq!(skin, ScreenState::SkinModal);
        assert!(!skin.is_result());

        f.bgr.fill(230);
        paste(&mut f, &screen.play_again.text, 217, 856);
        let board = screen.state(&f, &cfg).unwrap();
        assert_eq!(board, ScreenState::Board);
        assert!(board.playable(&f, &cfg, &screen, &d));

        // Waiting on a confirmed title does not require a dark backdrop.
        paste(&mut f, &screen.new_record.text, 247, 191);
        assert_eq!(screen.state(&f, &cfg).unwrap(), ScreenState::ResultPending);
        let bytes = fs::read(crate::asset_dir().join("templates/ui_restart.png")).unwrap();
        let restart = decode_mat(&bytes, imgcodecs::IMREAD_COLOR).unwrap();
        paste(&mut f, &restart, 200, 825);
        assert!(matches!(
            screen.state(&f, &cfg).unwrap(),
            ScreenState::Restart(_)
        ));
        paste(&mut f, &screen.skin_title.text, 219, 204);
        assert_eq!(screen.state(&f, &cfg).unwrap(), ScreenState::SkinModal);
    }

    #[test]
    fn phase_reports_signed_motion_and_rejects_untextured_frames() {
        let (width, height) = (80, 60);
        let a = Frame {
            width,
            height,
            bgr: (0..width * height)
                .flat_map(|i| {
                    let n = (i as u32).wrapping_mul(0x9e3779b9);
                    let [b, g, r, _] = (n ^ n.rotate_left(13)).to_le_bytes();
                    [b, g, r]
                })
                .collect(),
        };
        let mut b = Frame {
            width,
            height,
            bgr: vec![0; width * height * 3],
        };
        assert_eq!(b.shift(&b, 0.1), None);
        for y in 2..height {
            for x in 0..width - 3 {
                let src = ((y - 2) * width + x + 3) * 3;
                let out = (y * width + x) * 3;
                b.bgr[out..out + 3].copy_from_slice(&a.bgr[src..src + 3]);
            }
        }
        let (dx, dy) = a.shift(&b, 0.1).unwrap();
        assert!((dx + 3.).hypot(dy - 2.) < 0.3, "({dx}, {dy})");
    }

    #[test]
    fn has_dark_center_early_exit_matches_exhaustive_scan_at_exact_threshold() {
        let cfg = Config::default();
        let threshold = cfg.f("modal_dark_threshold") as i32;
        fn reference_dark_center(f: &Frame, threshold: i32) -> bool {
            let mut dark = 0;
            let mut count = 0;
            for y in f.height / 4..f.height * 65 / 100 {
                for x in f.width * 15 / 100..f.width * 85 / 100 {
                    let p = &f.bgr[(y * f.width + x) * 3..];
                    let g = (1868 * p[0] as i32 + 9617 * p[1] as i32 + 4899 * p[2] as i32 + 8192)
                        >> 14;
                    dark += usize::from(g < threshold);
                    count += 1;
                }
            }
            count > 0 && dark * 100 > 85 * count
        }
        let (w, h) = (40, 25);
        for bright_pixels in 0..=15 {
            let mut f = Frame {
                width: w,
                height: h,
                bgr: vec![0; w * h * 3],
            };
            let mut placed = 0;
            'outer: for y in h / 4..h * 65 / 100 {
                for x in w * 15 / 100..w * 85 / 100 {
                    if placed >= bright_pixels {
                        break 'outer;
                    }
                    let i = (y * w + x) * 3;
                    f.bgr[i..i + 3].fill((threshold + 10).min(255) as u8);
                    placed += 1;
                }
            }
            assert_eq!(
                f.has_dark_center(&cfg),
                reference_dark_center(&f, threshold),
                "bright_pixels={bright_pixels}"
            );
        }
    }

    #[test]
    fn transpose_blocked_handles_non_tile_aligned_dimensions() {
        for (rows, cols) in [(23, 37), (65, 33), (1, 70), (70, 1)] {
            let src: Vec<Complex32> = (0..rows * cols)
                .map(|i| Complex32::new(i as f32, -(i as f32)))
                .collect();
            let mut dst = vec![Complex32::default(); rows * cols];
            transpose_blocked(&src, &mut dst, rows, cols);
            for r in 0..rows {
                for c in 0..cols {
                    assert_eq!(
                        dst[c * rows + r],
                        src[r * cols + c],
                        "{rows}x{cols} ({r},{c})"
                    );
                }
            }
        }
    }

    #[test]
    fn button_reuses_cache_across_empty_hits_and_dimension_changes() {
        let dir = crate::asset_dir().join("templates");
        let screen = Screen::new(&dir, &Config::default()).unwrap();
        let start = Frame::read(&dir.join("ui_start.png")).unwrap();
        let restart = Frame::read(&dir.join("ui_restart.png")).unwrap();
        let make = |w, h, tpl: Option<(&Frame, usize, usize)>| {
            let mut frame = Frame {
                width: w,
                height: h,
                bgr: vec![90; w * h * 3],
            };
            if let Some((t, ox, oy)) = tpl {
                for (y, row) in t.bgr.chunks_exact(t.width * 3).enumerate() {
                    let off = ((oy + y) * w + ox) * 3;
                    frame.bgr[off..off + row.len()].copy_from_slice(row);
                }
            }
            frame
        };
        let (w1, h1) = (
            start.width.max(restart.width) + 61,
            start.height.max(restart.height) + 57,
        );
        assert_eq!(screen.button(&make(w1, h1, None)).unwrap(), None);
        assert_eq!(
            screen
                .button(&make(w1, h1, Some((&start, 11, 17))))
                .unwrap(),
            Some((
                false,
                [11. + start.width as f64 / 2., 17. + start.height as f64 / 2.]
            ))
        );
        assert_eq!(
            screen
                .button(&make(w1, h1, Some((&restart, 23, 9))))
                .unwrap(),
            Some((
                true,
                [
                    23. + restart.width as f64 / 2.,
                    9. + restart.height as f64 / 2.
                ]
            ))
        );
        assert_eq!(screen.button(&make(w1, h1, None)).unwrap(), None);
        let (w2, h2) = (w1 + 40, h1 + 30);
        assert_eq!(
            screen
                .button(&make(w2, h2, Some((&start, 19, 25))))
                .unwrap(),
            Some((
                false,
                [19. + start.width as f64 / 2., 25. + start.height as f64 / 2.]
            ))
        );
    }

    #[test]
    fn button_match_template_matches_direct_ncc_on_even_and_odd_sizes() {
        let dir = crate::asset_dir().join("templates");
        let screen = Screen::new(&dir, &Config::default()).unwrap();
        let start = Frame::read(&dir.join("ui_start.png")).unwrap();
        let restart = Frame::read(&dir.join("ui_restart.png")).unwrap();
        // Covers both single-threaded small frames and >=65_536 px WorkPool frames (even and odd dims).
        for (dw, dh, ox, oy) in [
            (41usize, 37usize, 13usize, 11usize),
            (42, 38, 16, 14),
            (80, 210, 47, 133),
            (81, 209, 52, 129),
        ] {
            for (tpl, is_restart) in [(&start, false), (&restart, true)] {
                let (w, h) = (tpl.width + dw, tpl.height + dh);
                let mut frame = Frame {
                    width: w,
                    height: h,
                    bgr: (0..w * h)
                        .flat_map(|i| {
                            let v = ((i * 17 + (i / w) * 9) % 180 + 20) as u8;
                            [v, v, v]
                        })
                        .collect(),
                };
                for (y, row) in tpl.bgr.chunks_exact(tpl.width * 3).enumerate() {
                    let off = ((oy + y) * w + ox) * 3;
                    frame.bgr[off..off + row.len()].copy_from_slice(row);
                }
                assert_eq!(
                    screen.button(&frame).unwrap(),
                    Some((
                        is_restart,
                        [
                            ox as f64 + tpl.width as f64 * 0.5,
                            oy as f64 + tpl.height as f64 * 0.5
                        ]
                    )),
                    "{w}x{h} restart={is_restart}"
                );
            }
        }

        // Direct scalar NCC comparison on a medium odd-sized textured frame without exact paste.
        let (w, h) = (275, 193);
        let frame = Frame {
            width: w,
            height: h,
            bgr: (0..w * h)
                .flat_map(|i| {
                    let v = ((i * 31 + (i / w) * 19) % 210 + 15) as u8;
                    [v, (v ^ 0x25), (v.wrapping_add(11))]
                })
                .collect(),
        };
        let frame_gray: Vec<u8> = frame.bgr.as_chunks::<3>().0.iter().map(|p| gray(p)).collect();
        let mut expected = None;
        let mut expected_best = 0.70f64;
        for (id, tpl) in screen.buttons.iter().enumerate() {
            let area = (tpl.w * tpl.h) as f64;
            let mut best = (f64::NEG_INFINITY, (0, 0));
            for y in 0..=h - tpl.h {
                for x in 0..=w - tpl.w {
                    let (mut sum, mut sum2, mut dot) = (0f64, 0f64, 0f64);
                    for ty in 0..tpl.h {
                        for tx in 0..tpl.w {
                            let p = f64::from(frame_gray[(y + ty) * w + x + tx]);
                            let t = f64::from(tpl.pixels[ty * tpl.w + tx]) - tpl.mean;
                            sum += p;
                            sum2 += p * p;
                            dot += p * t;
                        }
                    }
                    let var = sum2 - sum * sum / area;
                    if var > 0. {
                        let ncc = (dot / (tpl.norm * var.sqrt())).clamp(-1., 1.);
                        if ncc > best.0 {
                            best = (ncc, (x, y));
                        }
                    }
                }
            }
            if best.0 >= expected_best {
                expected_best = best.0;
                expected = Some((
                    id == 1,
                    [
                        best.1 .0 as f64 + tpl.w as f64 * 0.5,
                        best.1 .1 as f64 + tpl.h as f64 * 0.5,
                    ],
                ));
            }
        }
        assert_eq!(screen.button(&frame).unwrap(), expected);
    }

    #[test]
    fn screen_text_fft_matches_opencv_match_template() {
        fn opencv_text_center(st: &ScreenText, f: &Frame) -> Option<[f64; 2]> {
            let image = crate::template::bgr(f).unwrap();
            let a = st.area;
            let x = |n: i32| (n as f64 * f.width as f64 / 553.).round() as i32;
            let y = |n: i32| (n as f64 * f.height as f64 / 1050.).round() as i32;
            let bounds = Rect::new(
                x(a.x),
                y(a.y),
                x(a.x + a.width) - x(a.x),
                y(a.y + a.height) - y(a.y),
            );
            if bounds.empty() {
                return None;
            }
            let roi = Mat::roi(&image, bounds).unwrap();
            let mut scores = Mat::default();
            if bounds.width == a.width && bounds.height == a.height {
                imgproc::match_template_def(&roi, &st.text, &mut scores, imgproc::TM_CCOEFF_NORMED)
                    .unwrap();
            } else {
                let mut search = Mat::default();
                imgproc::resize(&roi, &mut search, a.size(), 0., 0., imgproc::INTER_LINEAR)
                    .unwrap();
                imgproc::match_template_def(
                    &search,
                    &st.text,
                    &mut scores,
                    imgproc::TM_CCOEFF_NORMED,
                )
                .unwrap();
            }
            let mut peak = 0.;
            let mut at = core::Point::default();
            core::min_max_loc(
                &scores,
                None,
                Some(&mut peak),
                None,
                Some(&mut at),
                &core::no_array(),
            )
            .unwrap();
            (peak >= 0.90).then(|| {
                [
                    bounds.x as f64
                        + (at.x as f64 + st.text.cols() as f64 / 2.) * bounds.width as f64
                            / a.width as f64,
                    bounds.y as f64
                        + (at.y as f64 + st.text.rows() as f64 / 2.) * bounds.height as f64
                            / a.height as f64,
                ]
            })
        }
        fn paste(f: &mut Frame, data: &Mat, x: usize, y: usize) {
            for (dy, row) in data
                .data_bytes()
                .unwrap()
                .chunks_exact(data.cols() as usize * 3)
                .enumerate()
            {
                let i = ((y + dy) * f.width + x) * 3;
                f.bgr[i..i + row.len()].copy_from_slice(row);
            }
        }
        fn assert_close(a: Option<[f64; 2]>, b: Option<[f64; 2]>, label: &str) {
            match (a, b) {
                (None, None) => {}
                (Some(pa), Some(pb)) => assert!(
                    (pa[0] - pb[0]).abs() < 1e-6 && (pa[1] - pb[1]).abs() < 1e-6,
                    "{label}: {pa:?} vs {pb:?}"
                ),
                _ => panic!("{label}: mismatch {a:?} vs {b:?}"),
            }
        }

        let cfg = Config::default();
        let root = crate::asset_dir();
        let screen = Screen::new(&root.join("templates"), &cfg).unwrap();

        for (w, h) in [(553usize, 1050usize), (552, 1049)] {
            let f = Frame {
                width: w,
                height: h,
                bgr: (0..w * h)
                    .flat_map(|i| {
                        let v = ((i * 29 + (i / w) * 17) % 160 + 25) as u8;
                        [v, v.wrapping_add(13), 255 - v]
                    })
                    .collect(),
            };
            for (name, st) in [
                ("empty_skin", &screen.skin_title),
                ("empty_again", &screen.play_again),
                ("empty_record", &screen.new_record),
            ] {
                assert_close(
                    st.center(&f).unwrap(),
                    opencv_text_center(st, &f),
                    &format!("{w}x{h} {name}"),
                );
            }
            for (name, st, px, py) in [
                ("hit_skin", &screen.skin_title, 218usize, 203usize),
                ("hit_again", &screen.play_again, 217usize, 855usize),
                ("hit_record", &screen.new_record, 246usize, 190usize),
            ] {
                let mut hit = f.clone();
                paste(&mut hit, &st.text, px, py);
                let got = st.center(&hit).unwrap();
                let exp = opencv_text_center(st, &hit);
                assert!(got.is_some(), "{w}x{h} {name} expected hit (exp={exp:?})");
                assert_close(got, exp, &format!("{w}x{h} {name}"));
            }
        }

        for name in ["skin-gold-play.png", "purple-cylinder-stall.png"] {
            let f = Frame::read(&root.join("tests/fixtures").join(name))
                .unwrap()
                .for_recognition(&cfg)
                .unwrap();
            for (label, st) in [
                ("skin", &screen.skin_title),
                ("again", &screen.play_again),
                ("record", &screen.new_record),
            ] {
                assert_close(
                    st.center(&f).unwrap(),
                    opencv_text_center(st, &f),
                    &format!("{name}:{label}"),
                );
            }
        }
    }
}
