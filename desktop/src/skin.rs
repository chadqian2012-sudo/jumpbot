use crate::Frame;
use opencv::{
    core::{self, Mat, Point, Point2f, Rect, Scalar, Size, ToInputArray, Vec3b, Vector},
    imgproc,
    prelude::*,
    Result as CvResult,
};

mod material;
type Contour = Vector<Point>;

struct Shape {
    bounds: Rect,
    contour: Contour,
    solidity: f64,
}
struct Head {
    bounds: Rect,
    hull: Contour,
    roundness: f64,
}
struct Platform {
    bounds: Rect,
    hull: Contour,
    area: f64,
}

fn diameter(b: Rect) -> f64 {
    f64::from(b.width + b.height) * 0.5
}
fn center_x(b: Rect) -> f64 {
    f64::from(b.x) + f64::from(b.width) * 0.5
}
fn round(n: f64) -> i32 {
    n.round_ties_even() as i32
}

#[derive(Debug, Clone)]
pub struct SkinMatch {
    pub x: f64,
    pub y: f64,
    pub score: f64,
    /// Left, top, width, height in the input frame.
    pub bbox: (usize, usize, usize, usize),
    /// Packed one-byte silhouette, local to bbox. Set pixels are 255.
    pub mask: Vec<u8>,
    /// Extra platform evidence in full-frame coordinates.
    pub support_points: Vec<(usize, usize)>,
}

#[derive(Default)]
struct Selection {
    height: i32,
    min_foot_ratio: f64,
    matches: Vec<(Rect, SkinMatch)>,
}

fn head_kind(width: i32, b: Rect, circularity: f64, solidity: f64) -> i32 {
    let aspect = f64::from(b.width) / f64::from(b.height);
    if !(8f64.max(f64::from(width) * 0.018)..=f64::from(width) * 0.10).contains(&f64::from(b.width))
        || solidity < 0.72
    {
        return 0;
    }
    if (0.80..=1.25).contains(&aspect) && circularity >= 0.84 {
        return 1;
    }
    // A flattened edge can seed a fresh material contour. That contour must
    // independently qualify as a complete head before it can seed a body.
    if (0.65..=1.55).contains(&aspect) && circularity >= 0.78 {
        return 2;
    }
    0
}

fn upright(selection: &Selection, h: Rect, b: Rect, solidity: f64) -> bool {
    let diameter = diameter(h);
    let gap = f64::from(b.y - h.br().y);
    let width = h.br().x.max(b.br().x) - h.x.min(b.x);
    let aspect = f64::from(b.br().y - h.y) / f64::from(width);
    (0.55..=1.65).contains(&(f64::from(b.width) / diameter))
        && (1.15..=3.8).contains(&(f64::from(b.height) / diameter))
        && solidity >= 0.86
        && f64::from(b.height) / f64::from(b.width) >= 1.15
        && (-2.0..=diameter * 0.35).contains(&gap)
        && (center_x(b) - center_x(h)).abs() <= diameter * 0.25
        && f64::from(b.br().y) >= f64::from(selection.height) * selection.min_foot_ratio
        && b.br().y < selection.height - 2
        && (1.5..=4.8).contains(&aspect)
}

fn median_with(values: &[i32], scratch: &mut Vec<i32>) -> f64 {
    scratch.clear();
    scratch.extend_from_slice(values);
    scratch.sort_unstable();
    f64::from(scratch[scratch.len() / 2] + scratch[(scratch.len() - 1) / 2]) * 0.5
}

fn has_waist(rows: &[i32], scratch: &mut Vec<i32>) -> bool {
    // A body aligned to a qualifying head has at least six rows.
    let h = rows.len();
    let neck = median_with(&rows[1.max(h / 8)..2.max(h / 3)], scratch);
    let base = median_with(&rows[h / 2..(h / 2 + 1).max(h * 7 / 8)], scratch);
    if neck >= 3.0 && base >= neck * 1.15 {
        return true;
    }
    let band = 3.max(h / 10);
    let collar = median_with(&rows[1.max(h / 12)..2.max(h / 6)], scratch);
    let waist = (h / 4..=(h / 2).min(h - band))
        .map(|y| median_with(&rows[y..y + band], scratch))
        .fold(f64::INFINITY, f64::min);
    let lower = (h / 2..=(h / 2).max(h * 7 / 8 - band))
        .map(|y| median_with(&rows[y..y + band], scratch))
        .fold(0.0, f64::max);
    (neck >= 3.0 && lower >= neck * 1.15)
        || (waist >= 3.0 && waist < collar && lower >= waist * 1.15)
}

fn nested(a: &(Rect, SkinMatch), b: &(Rect, SkinMatch)) -> bool {
    let (h, k) = (a.0, b.0);
    let overlap = (h.br().x.min(k.br().x) - h.x.max(k.x)).max(0)
        * (h.br().y.min(k.br().y) - h.y.max(k.y)).max(0);
    if f64::from(overlap) / f64::from(h.area() + k.area() - overlap) < 0.85 {
        return false;
    }
    let (mut small, mut large) = (&a.1, &b.1);
    if small.mask.len() > large.mask.len() {
        std::mem::swap(&mut small, &mut large);
    }
    let (x, y, w, h) = small.bbox;
    let (lx, ly, lw, lh) = large.bbox;
    if x < lx || y < ly || x + w > lx + lw || y + h > ly + lh {
        return false;
    }
    let mut occupied = 0;
    let mut covered = 0;
    for (dy, row) in small.mask.chunks_exact(w).enumerate() {
        let start = (y - ly + dy) * lw + x - lx;
        for (&p, &q) in row.iter().zip(&large.mask[start..start + w]) {
            if p != 0 {
                occupied += 1;
                covered += i32::from(q != 0);
            }
        }
    }
    occupied > 0 && f64::from(covered) >= 0.95 * f64::from(occupied)
}

fn consider(selection: &mut Selection, new: (Rect, SkinMatch)) {
    for old in &mut selection.matches {
        let enclosed = nested(old, &new);
        let close = (old.1.x - new.1.x).hypot(old.1.y - new.1.y) < diameter(new.0) * 0.35;
        if !enclosed && !close {
            continue;
        }
        // A longer complete outline wins over a high-scoring interior fragment.
        if (enclosed && new.1.y > old.1.y)
            || (new.1.score > old.1.score && (!enclosed || new.1.y == old.1.y))
        {
            *old = new;
        }
        return;
    }
    selection.matches.push(new);
}

#[derive(Default)]
pub(crate) struct SkinScratch {
    lab: Mat,
    gray: Mat,
    channels: Vector<Mat>,
    edge_bufs: [Vec<u8>; 4],
    pool: Option<crate::screen::WorkPool>,
    widths: Vec<i32>,
    median_buf: Vec<i32>,
}

fn count_nonzero_bytes(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut active = 0usize;
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") {
        use std::arch::x86_64::*;
        // AVX2 support is verified above and each 32-byte chunk stays inside `bytes`.
        unsafe {
            let zero = _mm256_setzero_si256();
            for chunk in bytes.as_chunks::<32>().0 {
                let v = _mm256_loadu_si256(chunk.as_ptr().cast());
                let eq = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, zero)) as u32;
                active += eq.count_zeros() as usize;
                offset += 32;
            }
        }
    }
    active + bytes[offset..].iter().filter(|&&b| b != 0).count()
}

#[cfg(all(test, target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn or_assign_avx2(dst: &mut [u8], src: &[u8]) -> usize {
    use std::arch::x86_64::*;
    let n = dst.len().min(src.len());
    let mut offset = 0usize;
    for (d, s) in dst[..n]
        .as_chunks_mut::<32>()
        .0
        .iter_mut()
        .zip(src[..n].as_chunks::<32>().0)
    {
        // SAFETY: AVX2 support is verified by caller and 32-byte slices stay in bounds.
        unsafe {
            let va = _mm256_loadu_si256(d.as_ptr().cast());
            let vb = _mm256_loadu_si256(s.as_ptr().cast());
            _mm256_storeu_si256(d.as_mut_ptr().cast(), _mm256_or_si256(va, vb));
        }
        offset += 32;
    }
    offset
}

#[cfg(test)]
fn or_assign_bytes(dst: &mut [u8], src: &[u8]) {
    let n = dst.len().min(src.len());
    let mut offset = 0usize;
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 support is verified above.
        offset = unsafe { or_assign_avx2(dst, src) };
    }
    let (dst_words, dst_tail) = dst[offset..n].as_chunks_mut::<8>();
    let (src_words, src_tail) = src[offset..n].as_chunks::<8>();
    for (d, s) in dst_words.iter_mut().zip(src_words) {
        let a = u64::from_ne_bytes(*d);
        let b = u64::from_ne_bytes(*s);
        *d = (a | b).to_ne_bytes();
    }
    for (d, &s) in dst_tail.iter_mut().zip(src_tail) {
        *d |= s;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn or4_avx2(dst: &mut [u8], a: &[u8], b: &[u8], c: &[u8]) -> usize {
    use std::arch::x86_64::*;
    let n = dst.len().min(a.len()).min(b.len()).min(c.len());
    let mut offset = 0usize;
    for (((d, sa), sb), sc) in dst[..n]
        .as_chunks_mut::<32>()
        .0
        .iter_mut()
        .zip(a[..n].as_chunks::<32>().0)
        .zip(b[..n].as_chunks::<32>().0)
        .zip(c[..n].as_chunks::<32>().0)
    {
        // SAFETY: AVX2 support is verified by caller and 32-byte slices stay in bounds.
        unsafe {
            let vd = _mm256_loadu_si256(d.as_ptr().cast());
            let va = _mm256_loadu_si256(sa.as_ptr().cast());
            let vb = _mm256_loadu_si256(sb.as_ptr().cast());
            let vc = _mm256_loadu_si256(sc.as_ptr().cast());
            let merged = _mm256_or_si256(_mm256_or_si256(vd, va), _mm256_or_si256(vb, vc));
            _mm256_storeu_si256(d.as_mut_ptr().cast(), merged);
        }
        offset += 32;
    }
    offset
}

fn or4_bytes(dst: &mut [u8], a: &[u8], b: &[u8], c: &[u8]) {
    let n = dst.len().min(a.len()).min(b.len()).min(c.len());
    let mut offset = 0usize;
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 support is verified above.
        offset = unsafe { or4_avx2(dst, a, b, c) };
    }
    let (d_words, d_tail) = dst[offset..n].as_chunks_mut::<8>();
    let (a_words, a_tail) = a[offset..n].as_chunks::<8>();
    let (b_words, b_tail) = b[offset..n].as_chunks::<8>();
    let (c_words, c_tail) = c[offset..n].as_chunks::<8>();
    for (((d, sa), sb), sc) in d_words.iter_mut().zip(a_words).zip(b_words).zip(c_words) {
        let merged = u64::from_ne_bytes(*d)
            | u64::from_ne_bytes(*sa)
            | u64::from_ne_bytes(*sb)
            | u64::from_ne_bytes(*sc);
        *d = merged.to_ne_bytes();
    }
    for (((d, &sa), &sb), &sc) in d_tail.iter_mut().zip(a_tail).zip(b_tail).zip(c_tail) {
        *d |= sa | sb | sc;
    }
}

/// An absent support mask requests shape-only recognition. With a mask,
/// even a geometrically valid person must have evidence of a platform.
#[cfg(test)]
pub fn detect_skin(
    frame: &Frame,
    support: Option<&[u8]>,
    min_foot_ratio: f64,
    allow_backdrop: bool,
) -> Option<SkinMatch> {
    detect_skin_with(
        frame,
        support,
        min_foot_ratio,
        allow_backdrop,
        &mut SkinScratch::default(),
    )
}

pub(crate) fn detect_skin_with(
    frame: &Frame,
    support: Option<&[u8]>,
    min_foot_ratio: f64,
    allow_backdrop: bool,
    scratch: &mut SkinScratch,
) -> Option<SkinMatch> {
    let width = i32::try_from(frame.width).ok()?;
    let height = i32::try_from(frame.height).ok()?;
    let pixels = frame.width.checked_mul(frame.height)?;
    if width.min(height) < 32
        || pixels > i32::MAX as usize
        || !frame.valid()
        || support.is_some_and(|mask| mask.len() != pixels)
        || !min_foot_ratio.is_finite()
    {
        return None;
    }
    let mut selection = Selection {
        height,
        min_foot_ratio,
        ..Selection::default()
    };
    visit(frame, support, allow_backdrop, &mut selection, scratch).ok()?;
    if selection.matches.len() != 1 {
        return None;
    }
    selection.matches.pop().map(|(_, m)| m)
}

fn contours(src: &impl ToInputArray, mode: i32, offset: Point) -> CvResult<Vector<Contour>> {
    let mut found = Vector::new();
    imgproc::find_contours(src, &mut found, mode, imgproc::CHAIN_APPROX_SIMPLE, offset)?;
    Ok(found)
}

fn hull(contour: &Contour) -> CvResult<(Contour, f64)> {
    let mut h = Contour::new();
    imgproc::convex_hull_def(contour, &mut h)?;
    let area = imgproc::contour_area_def(&h)?;
    Ok((h, area))
}

fn circularity(h: &Contour, area: f64) -> CvResult<f64> {
    let p = imgproc::arc_length(h, true)?;
    Ok(4. * std::f64::consts::PI * area / (p * p).max(1.))
}

fn filled(c: &Contour, bounds: Rect) -> CvResult<Mat> {
    let mut mask = Mat::zeros(bounds.height, bounds.width, core::CV_8U)?.to_mat()?;
    imgproc::draw_contours(
        &mut mask,
        &Vector::<Contour>::from_iter([c.clone()]),
        0,
        Scalar::all(255.),
        imgproc::FILLED,
        imgproc::LINE_8,
        &core::no_array(),
        0,
        Point::new(-bounds.x, -bounds.y),
    )?;
    Ok(mask)
}

fn erode(mask: &Mat) -> CvResult<Mat> {
    let mut inner = Mat::default();
    imgproc::erode_def(mask, &mut inner, &Mat::default())?;
    Ok(inner)
}

struct Scene<'a> {
    lab: &'a Mat,
    platforms: Vec<Platform>,
    support: Option<&'a [u8]>,
    width: i32,
    height: i32,
    top: i32,
}

impl Scene<'_> {
    fn support_under(
        &self,
        x: i32,
        y: i32,
        diameter: f64,
    ) -> CvResult<Option<Vec<(usize, usize)>>> {
        let Some(mask) = self.support else {
            return Ok(Some(Vec::new()));
        };
        let left = (x - 4).max(0);
        let right = (x + 5).min(self.width);
        let rows: Vec<_> = [6, 10, 14, 18, 24]
            .into_iter()
            .map(|d| y + d)
            .filter(|&py| py < self.height)
            .collect();
        let hits = rows
            .iter()
            .filter(|&&py| {
                let row = py as usize * self.width as usize;
                mask[row + left as usize..row + right as usize]
                    .iter()
                    .any(|&v| v != 0)
            })
            .count();
        if hits >= 2 {
            return Ok(Some(Vec::new()));
        }
        let mut best = Vec::new();
        for p in &self.platforms {
            if f64::from(p.bounds.width) < diameter * 2.
                || f64::from(p.bounds.height) < diameter
                || p.area < diameter * diameter * 2.
            {
                continue;
            }
            let mut points = Vec::new();
            let mut supported = 0;
            for &py in &rows {
                let start = points.len();
                for px in left..right {
                    if imgproc::point_polygon_test(
                        &p.hull,
                        Point2f::new(px as f32, (py - self.top) as f32),
                        false,
                    )? >= 0.
                    {
                        points.push((px as usize, py as usize));
                    }
                }
                if points.len() - start >= 5 {
                    supported += 1;
                } else {
                    points.truncate(start);
                }
            }
            if supported >= 2 && points.len() > best.len() {
                best = points;
            }
        }
        Ok((!best.is_empty()).then_some(best))
    }

    fn measure(
        &self,
        selection: &mut Selection,
        head: &Head,
        sample: &material::Material,
        body: &Shape,
        widths: &mut Vec<i32>,
        median_buf: &mut Vec<i32>,
    ) -> CvResult<bool> {
        let origin = Point::new(0, self.top);
        let b = body.bounds;
        let h = head.bounds;
        if !upright(selection, h + origin, b + origin, body.solidity) {
            return Ok(false);
        }
        let mask = filled(&body.contour, b)?;
        widths.clear();
        widths.extend(
            mask.data_bytes()?
                .chunks_exact(b.width as usize)
                .map(|row| count_nonzero_bytes(row) as i32),
        );
        if !has_waist(widths, median_buf) {
            return Ok(false);
        }
        let core = erode(&mask)?;
        let Some(colour) = material::median_colour_roi(
            self.lab,
            b,
            &core,
            Rect::new(0, 0, b.width, b.height),
        )?
        else {
            return Ok(false);
        };
        if material::distance(colour, sample.colour) > 35. {
            return Ok(false);
        }
        let rows = (round(diameter(h) * 0.25).max(3)).min(b.height);
        let Some(base) = material::median_colour_roi(
            self.lab,
            Rect::new(b.x, b.y + b.height - rows, b.width, rows),
            &core,
            Rect::new(0, b.height - rows, b.width, rows),
        )?
        else {
            return Ok(false);
        };
        if material::distance(base, sample.colour) > 35.
            && (base[1] - sample.colour[1]).hypot(base[2] - sample.colour[2]) > sample.tolerance
        {
            return Ok(false);
        }
        let foot_y = b.br().y + self.top;
        let Some(points) = self.support_under(round(center_x(b)), foot_y, diameter(h))? else {
            return Ok(false);
        };
        let bounds = h | b;
        let mut silhouette = vec![0; bounds.area() as usize];
        for (part, pixels) in [(h, &sample.mask), (b, &mask)] {
            for (dy, row) in pixels
                .data_bytes()?
                .chunks_exact(part.width as usize)
                .enumerate()
            {
                let start = ((part.y - bounds.y) as usize + dy) * bounds.width as usize
                    + (part.x - bounds.x) as usize;
                for (dst, &v) in silhouette[start..].iter_mut().zip(row) {
                    *dst |= v;
                }
            }
        }
        consider(
            selection,
            (
                h + origin,
                SkinMatch {
                    x: center_x(b),
                    y: f64::from(foot_y),
                    score: head.roundness.min(body.solidity),
                    bbox: (
                        bounds.x as usize,
                        (bounds.y + self.top) as usize,
                        bounds.width as usize,
                        bounds.height as usize,
                    ),
                    mask: silhouette,
                    support_points: points,
                },
            ),
        );
        Ok(true)
    }
}

fn visit(
    frame: &Frame,
    support: Option<&[u8]>,
    allow_backdrop: bool,
    selection: &mut Selection,
    scratch: &mut SkinScratch,
) -> CvResult<()> {
    let (width, height) = (frame.width as i32, frame.height as i32);
    if let Some(m) = support {
        let active = count_nonzero_bytes(m);
        if active == 0 || active as f64 / m.len() as f64 > 0.90 {
            return Ok(());
        }
    }
    let top = (f64::from(height) * 0.35) as i32;
    let crop_rows = height - top;
    let crop_cols = width;
    let borrowed = crate::template::bgr(frame)?;
    let crop = borrowed.roi(Rect::new(0, top, width, crop_rows))?;
    imgproc::cvt_color_def(&crop, &mut scratch.lab, imgproc::COLOR_BGR2Lab)?;
    imgproc::cvt_color_def(&crop, &mut scratch.gray, imgproc::COLOR_BGR2GRAY)?;
    core::split(&scratch.lab, &mut scratch.channels)?;
    let crop_len = crop_rows as usize * crop_cols as usize;
    for buf in &mut scratch.edge_bufs {
        buf.resize(crop_len, 0);
    }
    let ensure_cont = |m: Mat| -> CvResult<Mat> {
        if m.is_continuous() {
            Ok(m)
        } else {
            m.try_clone()
        }
    };
    let gray_cont = if scratch.gray.is_continuous() {
        None
    } else {
        Some(scratch.gray.try_clone()?)
    };
    let gray_ref = gray_cont.as_ref().unwrap_or(&scratch.gray);
    let ch0 = ensure_cont(scratch.channels.get(0)?)?;
    let ch1 = ensure_cont(scratch.channels.get(1)?)?;
    let ch2 = ensure_cont(scratch.channels.get(2)?)?;
    let [e0, e1, e2, e3] = &mut scratch.edge_bufs;
    struct EdgeTask<'a> {
        src: &'a [u8],
        dst: &'a mut [u8],
        t1: f64,
        t2: f64,
        err: Option<opencv::Error>,
    }
    let mut tasks = [
        EdgeTask {
            src: gray_ref.data_bytes()?,
            dst: e0.as_mut_slice(),
            t1: 25.,
            t2: 70.,
            err: None,
        },
        EdgeTask {
            src: ch0.data_bytes()?,
            dst: e1.as_mut_slice(),
            t1: 20.,
            t2: 60.,
            err: None,
        },
        EdgeTask {
            src: ch1.data_bytes()?,
            dst: e2.as_mut_slice(),
            t1: 20.,
            t2: 60.,
            err: None,
        },
        EdgeTask {
            src: ch2.data_bytes()?,
            dst: e3.as_mut_slice(),
            t1: 20.,
            t2: 60.,
            err: None,
        },
    ];
    let run_canny = |_i: usize, task: &mut EdgeTask<'_>| {
        let res = (|| -> CvResult<()> {
            let src_mat = Mat::new_rows_cols_with_bytes::<u8>(crop_rows, crop_cols, task.src)?;
            let mut dst_mat =
                Mat::new_rows_cols_with_bytes_mut::<u8>(crop_rows, crop_cols, task.dst)?;
            imgproc::canny_def(&src_mat, &mut dst_mat, task.t1, task.t2)?;
            Ok(())
        })();
        if let Err(e) = res {
            task.err = Some(e);
        }
    };
    if crop_len >= 32_768 {
        let pool = scratch
            .pool
            .get_or_insert_with(|| crate::screen::WorkPool::new(3));
        pool.for_each_mut(&mut tasks, run_canny);
    } else {
        for (i, task) in tasks.iter_mut().enumerate() {
            run_canny(i, task);
        }
    }
    for task in tasks {
        if let Some(e) = task.err {
            return Err(e);
        }
    }
    or4_bytes(e0, e1, e2, e3);
    let edges_mat = Mat::new_rows_cols_with_bytes::<u8>(crop_rows, crop_cols, e0)?;
    let (mut heads, mut shapes, mut platforms) = (Vec::new(), Vec::new(), Vec::new());
    let head_range = 8f64.max(f64::from(width) * 0.018)..=f64::from(width) * 0.10;
    for c in contours(&edges_mat, imgproc::RETR_LIST, Point::default())? {
        let b = imgproc::bounding_rect(&c)?;
        if b.x <= 1
            || b.br().x >= width - 1
            || b.width < 5
            || b.height < 5
            || b.width * b.height < 20
        {
            continue;
        }
        let (h, area) = hull(&c)?;
        if area < 20. {
            continue;
        }
        let solidity = imgproc::contour_area_def(&c)? / area;
        shapes.push(Shape {
            bounds: b,
            contour: c,
            solidity,
        });
        if f64::from(b.width) >= f64::from(width) * 0.08
            && area >= f64::from(width) * f64::from(height) * 0.003
        {
            platforms.push(Platform {
                bounds: b,
                hull: h.clone(),
                area,
            });
        }
        if solidity < 0.72 || !head_range.contains(&f64::from(b.width)) {
            continue;
        }
        let roundness = circularity(&h, area)?;
        match head_kind(width, b, roundness, solidity) {
            1 => heads.push(Head {
                bounds: b,
                hull: h,
                roundness,
            }),
            2 => {
                for recovered in material::recover_heads(&scratch.lab, &h, b)? {
                    let (h, area) = hull(&recovered)?;
                    if area < 20. {
                        continue;
                    }
                    let r = imgproc::bounding_rect(&recovered)?;
                    let roundness = circularity(&h, area)?;
                    if head_kind(
                        width,
                        r,
                        roundness,
                        imgproc::contour_area_def(&recovered)? / area,
                    ) == 1
                    {
                        heads.push(Head {
                            bounds: r,
                            hull: h,
                            roundness,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let SkinScratch {
        lab,
        widths,
        median_buf,
        ..
    } = scratch;
    let scene = Scene {
        lab,
        platforms,
        support,
        width,
        height,
        top,
    };
    let mut sampled = Vec::with_capacity(heads.len());
    for h in &heads {
        if let Some(sample) = material::sample_head(scene.lab, &h.hull, h.bounds)? {
            sampled.push((h, sample));
        }
    }
    for backdrop in [false, true] {
        // Never retry an ambiguous first pass with the backdrop peeled away.
        if backdrop && (!allow_backdrop || !selection.matches.is_empty()) {
            break;
        }
        for (h, sample) in &sampled {
            let mut original_found = false;
            if !backdrop {
                for body in &shapes {
                    original_found |= scene.measure(selection, h, sample, body, widths, median_buf)?;
                }
            }
            material::visit_bodies(
                scene.lab,
                h.bounds,
                sample,
                backdrop,
                original_found,
                |body| scene.measure(selection, h, sample, body, widths, median_buf),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Frame {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(format!("{name}.png"));
        Frame::read(&path).unwrap()
    }

    #[test]
    fn standing_feet_stay_on_the_labelled_character() {
        // Foot labels from the existing real-image regressions (pixel units).
        for (name, x, y, tolerance) in [
            ("skin-gold-play", 173., 647., 2.),
            ("skin-gold-red-platform", 350., 613., 2.),
            ("skin-gold-green-platform", 407., 657., 2.),
            ("skin-gold-cardboard", 391., 643., 3.),
            ("skin-gold-white-book", 150., 659., 3.),
            ("skin-gold-start-texture", 173., 645., 2.),
            ("skin-gold-round-platform", 232., 611., 2.),
            ("skin-gold-yellow-platform", 356., 618., 2.),
            ("skin-gold-tan-background", 175., 646., 2.),
            ("skin-gold-narrow-pylon", 165., 650., 2.),
            ("skin-gold-yellow-badge", 364., 626., 2.),
            ("skin-gold-cardboard-seam", 338., 606., 1.),
            ("skin-gold-cardboard-bonus", 200., 630., 1.),
            ("purple-cylinder-stall", 264., 593., 2.),
        ] {
            let m = detect_skin(&fixture(name), None, 0.5, true).expect(name);
            assert!(
                (m.x - x).abs() <= tolerance && (m.y - y).abs() <= tolerance,
                "{name}: foot ({}, {})",
                m.x,
                m.y
            );
            let (_, top, width, height) = m.bbox;
            assert_eq!(m.mask.len(), width * height, "{name}");
            assert_eq!(top + height, m.y as usize, "{name}");
            assert!(
                m.mask[..width].contains(&255) && m.mask[(height - 1) * width..].contains(&255)
            );
            assert!((0.84..=1.).contains(&m.score), "{name}: {}", m.score);
        }
    }

    #[test]
    fn a_dark_shadow_touching_the_gold_foot_stays_off_the_silhouette() {
        let mut frame = fixture("skin-gold-play");
        for dx in 0..77 {
            let start = 645 + dx / 4;
            for y in start..start + 4 {
                let i = (y * frame.width + 172 + dx) * 3;
                frame.bgr[i..i + 3].copy_from_slice(&[60, 63, 65]);
            }
        }
        let m = detect_skin(&frame, None, 0.5, true).unwrap();
        assert_eq!((m.x, m.y), (173., 646.));
        assert_eq!(m.bbox, (153, 538, 40, 108));
    }

    #[test]
    fn incomplete_or_competing_silhouettes_have_no_winner() {
        let mut frame = fixture("skin-gold-yellow-badge");
        for row in 518..628 {
            let start = (row * frame.width + 345) * 3;
            frame
                .bgr
                .copy_within(start..start + 40 * 3, (row * frame.width + 145) * 3);
        }
        assert!(
            detect_skin(&frame, None, 0.5, true).is_none(),
            "two complete people"
        );
        let mut frame = fixture("skin-gold-yellow-badge");
        let background = frame.bgr[(100 * frame.width + 460) * 3..][..3].to_vec();
        for row in 575..611 {
            for x in 345..385 {
                frame.bgr[(row * frame.width + x) * 3..][..3].copy_from_slice(&background);
            }
        }
        assert!(
            detect_skin(&frame, None, 0.5, true).is_none(),
            "missing body"
        );
    }

    #[test]
    fn support_requires_a_valid_mask_and_two_rows_under_one_foot() {
        let frame = fixture("skin-gold-narrow-pylon");
        let mut support = vec![0; frame.width * frame.height];
        assert!(detect_skin(&frame, Some(&support), 0.5, true).is_none());
        support.fill(255);
        assert!(detect_skin(&frame, Some(&support), 0.5, true).is_none());
        support.fill(0);
        support[(649 + 6) * frame.width + 166] = 255;
        assert!(detect_skin(&frame, Some(&support), 0.5, true).is_none());
        support[(649 + 10) * frame.width + 166] = 255;
        let m = detect_skin(&frame, Some(&support), 0.5, true).unwrap();
        assert_eq!((m.x, m.y), (166., 649.));
        assert!(m.support_points.is_empty());

        // A broad outlined platform can fill in precisely the missing rows.
        let frame = fixture("skin-gold-play");
        let mut outside = vec![0; frame.width * frame.height];
        outside[0] = 255;
        let m = detect_skin(&frame, Some(&outside), 0.5, true).unwrap();
        let xs = m.x.round_ties_even() as usize;
        let ys = m.y as usize;
        let expected: Vec<_> = [6, 10, 14, 18, 24]
            .into_iter()
            .flat_map(|dy| (xs - 4..=xs + 4).map(move |x| (x, ys + dy)))
            .collect();
        assert_eq!(m.support_points, expected);
    }

    #[test]
    fn resampled_skins_keep_their_feet_and_reject_falls() {
        let reference = crate::Config::default();
        let mut vision = crate::vision::Vision::new(&reference, "simd").unwrap();
        for (name, width, height, foot) in [
            ("skin-gold-cardboard-bonus", 360, 684, Some((200., 630.))),
            ("skin-gold-cardboard-seam", 360, 684, Some((338., 606.))),
            ("skin-gold-cardboard", 360, 684, Some((391., 643.))),
            ("skin-gold-yellow-badge", 830, 1575, Some((364., 626.))),
            ("skin-gold-falling", 360, 684, None),
            ("purple-horizontal-fall", 830, 1575, None),
        ] {
            let mut capture_size = reference.clone();
            capture_size.0["window_ref_width"] = width.into();
            capture_size.0["window_ref_height"] = height.into();
            let capture = fixture(name).for_recognition(&capture_size).unwrap();
            let frame = capture.for_recognition(&reference).unwrap();
            vision.reset_prior();
            let result = vision.detect(&frame).unwrap();
            if let Some((x, y)) = foot {
                let piece = result.piece.expect(name);
                assert_eq!(piece.source, "skin-shape", "{name}");
                assert!((piece.x - x).hypot(piece.y - y) <= 4., "{name}: {piece:?}");
                assert!(result.target.is_some(), "{name}");
            } else {
                assert!(result.piece.is_none() && result.target.is_none(), "{name}");
            }
        }
    }

    #[test]
    fn backdrop_recovery_keeps_the_complete_resampled_cardboard_silhouette() {
        let cfg = crate::Config::default();
        let mut small = cfg.clone();
        small.0["window_ref_width"] = 360.into();
        small.0["window_ref_height"] = 684.into();
        // Exact shapes recorded from the pre-migration C++ implementation.
        for (name, foot, bounds, crc) in [
            (
                "skin-gold-cardboard-bonus",
                (200., 631.),
                (180, 523, 40, 108),
                0xc1377bc2,
            ),
            (
                "skin-gold-cardboard-seam",
                (338., 609.),
                (318, 499, 40, 110),
                0x929453b4,
            ),
        ] {
            let frame = fixture(name)
                .for_recognition(&small)
                .unwrap()
                .for_recognition(&cfg)
                .unwrap();
            assert!(detect_skin(&frame, None, 0.5, false).is_none(), "{name}");
            let m = detect_skin(&frame, None, 0.5, true).expect(name);
            assert_eq!(
                ((m.x, m.y), m.bbox, crc32fast::hash(&m.mask)),
                (foot, bounds, crc),
                "{name}"
            );
        }
    }

    #[test]
    fn falling_frames_cannot_confirm_a_landing_after_an_upright_piece() {
        let mut vision = crate::vision::Vision::new(&crate::Config::default(), "cpu").unwrap();
        for (standing, falling) in [
            ("purple-cylinder-stall", "purple-horizontal-fall"),
            ("skin-gold-play", "skin-gold-falling"),
        ] {
            assert!(
                vision
                    .detect_piece_only(&fixture(standing))
                    .unwrap()
                    .piece
                    .is_some(),
                "{standing}"
            );
            assert!(
                vision
                    .detect_piece_only(&fixture(falling))
                    .unwrap()
                    .piece
                    .is_none(),
                "{falling}"
            );
        }
    }

    #[test]
    fn count_nonzero_bytes_matches_scalar_across_tail_lengths() {
        for len in [0usize, 1, 15, 31, 32, 33, 63, 64, 65, 100] {
            let bytes: Vec<u8> = (0..len)
                .map(|i| if i % 3 == 0 { 255 } else { 0 })
                .collect();
            let expected = bytes.iter().filter(|&&b| b != 0).count();
            assert_eq!(count_nonzero_bytes(&bytes), expected, "len={len}");
        }
    }

    #[test]
    fn or_assign_bytes_matches_scalar_across_tail_lengths() {
        for len in [0usize, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 65] {
            let mut dst: Vec<u8> = (0..len).map(|i| ((i * 19) % 256) as u8).collect();
            let src: Vec<u8> = (0..len).map(|i| ((i * 41 + 7) % 256) as u8).collect();
            let expected: Vec<u8> = dst.iter().zip(&src).map(|(&a, &b)| a | b).collect();
            or_assign_bytes(&mut dst, &src);
            assert_eq!(dst, expected, "len={len}");
        }
    }

    #[test]
    fn parallel_canny_or4_matches_sequential_canny_edges() {
        for len in [0usize, 1, 7, 8, 15, 31, 32, 33, 63, 64, 65, 99] {
            let mut dst: Vec<u8> = (0..len).map(|i| ((i * 13) % 256) as u8).collect();
            let a: Vec<u8> = (0..len).map(|i| ((i * 29 + 3) % 256) as u8).collect();
            let b: Vec<u8> = (0..len).map(|i| ((i * 37 + 11) % 256) as u8).collect();
            let c: Vec<u8> = (0..len).map(|i| ((i * 53 + 19) % 256) as u8).collect();
            let expected: Vec<u8> = (0..len).map(|i| dst[i] | a[i] | b[i] | c[i]).collect();
            or4_bytes(&mut dst, &a, &b, &c);
            assert_eq!(dst, expected, "or4 len={len}");
        }

        let frames = [
            fixture("skin-gold-play"),
            Frame {
                width: 243,
                height: 301,
                bgr: (0..243 * 301)
                    .flat_map(|i| {
                        let v = ((i * 29 + (i / 243) * 17) % 220 + 15) as u8;
                        [v, v.wrapping_mul(3), 255 - v]
                    })
                    .collect(),
            },
        ];
        let mut scratch = SkinScratch::default();
        for frame in &frames {
            let (width, height) = (frame.width as i32, frame.height as i32);
            let top = (f64::from(height) * 0.35) as i32;
            let borrowed = crate::template::bgr(frame).unwrap();
            let crop = borrowed.roi(Rect::new(0, top, width, height - top)).unwrap();
            let (mut seq_lab, mut seq_gray, mut seq_edges, mut seq_edge) =
                (Mat::default(), Mat::default(), Mat::default(), Mat::default());
            let mut seq_channels = Vector::<Mat>::new();
            imgproc::cvt_color_def(&crop, &mut seq_lab, imgproc::COLOR_BGR2Lab).unwrap();
            imgproc::cvt_color_def(&crop, &mut seq_gray, imgproc::COLOR_BGR2GRAY).unwrap();
            imgproc::canny_def(&seq_gray, &mut seq_edges, 25., 70.).unwrap();
            core::split(&seq_lab, &mut seq_channels).unwrap();
            let combined = seq_edges.data_bytes_mut().unwrap();
            for i in 0..seq_channels.len() {
                imgproc::canny_def(&seq_channels.get(i).unwrap(), &mut seq_edge, 20., 60.).unwrap();
                or_assign_bytes(combined, seq_edge.data_bytes().unwrap());
            }

            let mut selection = Selection {
                height,
                min_foot_ratio: 0.5,
                ..Selection::default()
            };
            visit(frame, None, true, &mut selection, &mut scratch).unwrap();
            assert_eq!(
                scratch.edge_bufs[0].as_slice(),
                seq_edges.data_bytes().unwrap(),
                "{}x{}",
                frame.width,
                frame.height
            );
        }
    }
}
