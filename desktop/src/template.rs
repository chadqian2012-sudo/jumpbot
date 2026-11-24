use crate::Frame;
use opencv::{
    core::{self, Mat, Point, Rect, Size, Vec3b},
    imgproc,
    prelude::*,
};

pub(crate) fn bgr(frame: &Frame) -> opencv::Result<opencv::boxed_ref::BoxedRef<'_, Mat>> {
    Mat::new_rows_cols_with_bytes::<Vec3b>(frame.height as i32, frame.width as i32, &frame.bgr)
}

fn centered(mat: &Mat) -> opencv::Result<Mat> {
    let mono = mat.reshape(1, 0)?;
    let mut float = Mat::default();
    mono.convert_to(&mut float, core::CV_32F, 1., 0.)?;
    let mut centered = Mat::default();
    core::subtract_def(&float, &core::mean_def(&float)?, &mut centered)?;
    Ok(centered)
}

pub(crate) struct PieceTemplate {
    pub(crate) frame: Frame,
    centered_row: Mat,
    norm: f64,
}

impl PieceTemplate {
    pub(crate) fn new(frame: Frame) -> opencv::Result<Self> {
        let b = centered(&Mat::from_slice(&frame.bgr)?.clone_pointee())?;
        let norm = core::norm_def(&b)?;
        let centered_row = b.reshape(1, 1)?.try_clone()?;
        Ok(Self {
            frame,
            centered_row,
            norm,
        })
    }
}

/// Compare a resampled candidate and a learned BGR template with one mean across channels.
pub(crate) fn score(frame: &Frame, bounds: [i32; 4], tpl: &PieceTemplate) -> opencv::Result<f64> {
    let image = bgr(frame)?;
    let [x, y, w, h] = bounds;
    let roi = Mat::roi(&image, Rect::new(x, y, w, h))?;
    let mut resized = Mat::default();
    imgproc::resize(
        &roi,
        &mut resized,
        Size::new(tpl.frame.width as i32, tpl.frame.height as i32),
        0.,
        0.,
        imgproc::INTER_LINEAR,
    )?;
    let a = centered(&resized)?;
    Ok(a.reshape(1, 1)?.dot(&tpl.centered_row)? / (core::norm_def(&a)? * tpl.norm + 1e-6))
}

pub(crate) fn scan(frame: &Frame, top: usize, tpl: &Frame) -> opencv::Result<(i32, i32, f64)> {
    let image = bgr(frame)?;
    let search = Mat::roi(
        &image,
        Rect::new(
            0,
            top as i32,
            frame.width as i32,
            (frame.height - top) as i32,
        ),
    )?;
    let mut scores = Mat::default();
    imgproc::match_template_def(&search, &bgr(tpl)?, &mut scores, imgproc::TM_CCOEFF_NORMED)?;
    let mut point = Point::default();
    let mut max = 0.;
    core::min_max_loc(
        &scores,
        None,
        Some(&mut max),
        None,
        Some(&mut point),
        &core::no_array(),
    )?;
    Ok((point.x, point.y + top as i32, max))
}
