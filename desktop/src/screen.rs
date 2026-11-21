use crate::{Config, Frame, Result};
use std::{
    ffi::{c_int, c_void},
    fs,
    path::Path,
    ptr::NonNull,
};
type Pixels = extern "C" fn(*mut c_void, c_int, c_int, *const u8);
extern "C" {
    fn jb_resize_bgr(
        data: *const u8,
        w: c_int,
        h: c_int,
        out: *mut u8,
        out_w: c_int,
        out_h: c_int,
    ) -> c_int;
    fn jb_decode(bytes: *const u8, size: usize, ctx: *mut c_void, cb: Pixels) -> c_int;
    fn jb_encode(bytes: *const u8, w: c_int, h: c_int, ctx: *mut c_void, cb: Pixels) -> c_int;
    fn jb_templates() -> *mut c_void;
    fn jb_templates_drop(ctx: *mut c_void);
    fn jb_template(ctx: *mut c_void, id: c_int, data: *const u8, size: c_int) -> c_int;
    fn jb_digit_scores(
        ctx: *mut c_void,
        data: *const u8,
        w: c_int,
        h: c_int,
        rect: *const i32,
        output: *mut c_void,
        emit: extern "C" fn(*mut c_void, *const [f64; 10]),
    ) -> c_int;
    fn jb_button_scores(
        ctx: *mut c_void,
        data: *const u8,
        w: c_int,
        h: c_int,
        out: *mut [f64; 3],
    ) -> c_int;
    fn jb_motion(data: *const u8, w: c_int, h: c_int, top: c_int, out: *mut u8) -> c_int;
    fn jb_flow(before: *const u8, after: *const u8, w: c_int, h: c_int, out: *mut f64) -> c_int;
    fn jb_cv_threads(n: c_int);
}
impl Frame {
    /// Keep templates, axis projection and calibration in one coordinate system.
    pub(crate) fn resized_for_recognition(&self, cfg: &Config) -> Result<Option<Self>> {
        let width = cfg.i("window_ref_width") as usize;
        let height = cfg.i("window_ref_height") as usize;
        let tolerance = cfg.i("window_size_tol_px") as usize;
        if self.width.abs_diff(width) <= tolerance && self.height.abs_diff(height) <= tolerance {
            return Ok(None);
        }
        if !self.valid() {
            return Err("截图像素无效".into());
        }
        let mut bgr = vec![0; width * height * 3];
        if unsafe {
            jb_resize_bgr(
                self.bgr.as_ptr(),
                self.width as _,
                self.height as _,
                bgr.as_mut_ptr(),
                width as _,
                height as _,
            )
        } == 0
        {
            return Err("无法将客户区截图换算到标定尺寸".into());
        }
        Ok(Some(Self { width, height, bgr }))
    }

    pub(crate) fn for_recognition(self, cfg: &Config) -> Result<Self> {
        Ok(self.resized_for_recognition(cfg)?.unwrap_or(self))
    }

    pub fn read(path: &Path) -> Result<Self> {
        extern "C" fn copy(ctx: *mut c_void, w: i32, h: i32, p: *const u8) {
            unsafe {
                *ctx.cast::<Option<Frame>>() = Some(Frame {
                    width: w as usize,
                    height: h as usize,
                    bgr: std::slice::from_raw_parts(p, w as usize * h as usize * 3).to_vec(),
                });
            }
        }
        let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut frame: Option<Self> = None;
        unsafe {
            jb_decode(
                bytes.as_ptr(),
                bytes.len(),
                (&mut frame as *mut Option<Self>).cast(),
                copy,
            );
        }
        frame.ok_or_else(|| format!("无法解码 {}", path.display()))
    }
    pub fn write(&self, path: &Path) -> Result<()> {
        extern "C" fn copy(ctx: *mut c_void, n: i32, _: i32, p: *const u8) {
            unsafe {
                (*ctx.cast::<Vec<u8>>())
                    .extend_from_slice(std::slice::from_raw_parts(p, n as usize));
            }
        }
        let mut data = Vec::new();
        if !self.valid()
            || unsafe {
                jb_encode(
                    self.bgr.as_ptr(),
                    self.width as _,
                    self.height as _,
                    (&mut data as *mut Vec<u8>).cast(),
                    copy,
                )
            } == 0
        {
            return Err("PNG 编码失败".into());
        }
        fs::write(path, data).map_err(|e| e.to_string())
    }
    pub fn motion_sample(&self, cfg: &Config) -> Result<Vec<u8>> {
        let mut out = vec![0; 96 * 176];
        let top = (cfg.f("top_bar_ratio") * self.height as f64).round_ties_even() as i32;
        if !self.valid()
            || unsafe {
                jb_motion(
                    self.bgr.as_ptr(),
                    self.width as _,
                    self.height as _,
                    top,
                    out.as_mut_ptr(),
                )
            } == 0
        {
            return Err("无法计算画面变化".into());
        }
        Ok(out)
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
            || self.width != after.width
            || self.height != after.height
        {
            return None;
        }
        let mut result = [0.; 3];
        let ok = unsafe {
            jb_flow(
                self.bgr.as_ptr(),
                after.bgr.as_ptr(),
                self.width as _,
                self.height as _,
                result.as_mut_ptr(),
            )
        };
        (ok != 0 && result[2] >= min_response).then_some((result[0], result[1]))
    }
}
pub struct Screen(NonNull<c_void>);
impl Drop for Screen {
    fn drop(&mut self) {
        unsafe { jb_templates_drop(self.0.as_ptr()) }
    }
}
impl Screen {
    pub fn new(path: &Path, cfg: &Config) -> Result<Self> {
        let threads = if cfg.i("cv2_threads") <= 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) as i32
        } else {
            cfg.i("cv2_threads").min(128) as i32
        };
        unsafe {
            jb_cv_threads(threads);
        }
        let screen = Self(NonNull::new(unsafe { jb_templates() }).ok_or("模板初始化失败")?);
        for i in 0..12 {
            let name = match i {
                10 => "ui_start.png".into(),
                11 => "ui_restart.png".into(),
                _ => format!("digit_{i}.png"),
            };
            let bytes = fs::read(path.join(&name)).map_err(|e| format!("{name}: {e}"))?;
            if unsafe { jb_template(screen.0.as_ptr(), i, bytes.as_ptr(), bytes.len() as i32) } == 0
            {
                return Err(format!("模板损坏 {name}"));
            }
        }
        Ok(screen)
    }
    pub fn score(&self, f: &Frame, c: &Config) -> Option<i32> {
        if !f.valid() {
            return None;
        }
        let r = [
            (c.at("score_roi", 0) * f.width as f64) as i32,
            (c.at("score_roi", 1) * f.height as f64) as i32,
            (c.at("score_roi", 2) * f.width as f64) as i32,
            (c.at("score_roi", 3) * f.height as f64) as i32,
        ];
        extern "C" fn digit(ctx: *mut c_void, scores: *const [f64; 10]) {
            // OpenCV invokes this synchronously once per glyph, from left to right.
            let (score, scores) = unsafe { (&mut *ctx.cast::<Option<i32>>(), &*scores) };
            let Some(total) = *score else { return };
            let best = (1..10).fold(0, |best, i| if scores[i] > scores[best] { i } else { best });
            *score = (scores[best] >= 0.80)
                .then_some(best as i32)
                .and_then(|digit| total.checked_mul(10)?.checked_add(digit));
        }
        let mut total = Some(0);
        let ok = unsafe {
            jb_digit_scores(
                self.0.as_ptr(),
                f.bgr.as_ptr(),
                f.width as _,
                f.height as _,
                r.as_ptr(),
                (&mut total as *mut Option<i32>).cast(),
                digit,
            )
        };
        if ok == 0 {
            None
        } else {
            total
        }
    }
    pub fn button(&self, f: &Frame) -> Result<Option<(bool, [f64; 2])>> {
        if !f.valid() {
            return Err("图像缓冲区尺寸无效".into());
        }
        let mut candidates = [[-1.0, 0., 0.]; 2];
        if unsafe {
            jb_button_scores(
                self.0.as_ptr(),
                f.bgr.as_ptr(),
                f.width as _,
                f.height as _,
                candidates.as_mut_ptr(),
            )
        } == 0
        {
            return Err("界面识别失败".into());
        }
        // Match the original sorted templates: restart wins an exact tie.
        let restart = candidates[1][0] >= candidates[0][0];
        let [similarity, x, y] = candidates[usize::from(restart)];
        Ok((similarity >= 0.70).then_some((restart, [x, y])))
    }
}
