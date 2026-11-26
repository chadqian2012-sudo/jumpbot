//! Frame-local Lab colour and texture proposals for one validated head.
use super::*;
use std::collections::BTreeMap;

pub(super) struct Material {
    pub mask: Mat,
    core: Mat,
    pub colour: [f64; 3],
    pub tolerance: f64,
}

fn median_bin(bins: &[i32; 256], count: i32) -> f64 {
    let mut cumulative = 0;
    let mut lower = None;
    for (v, n) in bins.iter().enumerate() {
        cumulative += n;
        if cumulative > (count - 1) / 2 {
            lower.get_or_insert(v);
        }
        if cumulative > count / 2 {
            return (lower.unwrap() + v) as f64 * 0.5;
        }
    }
    unreachable!("histogram count mismatch")
}

pub(super) fn median_colour(
    lab: &impl MatTraitConst,
    core: &impl MatTraitConst,
) -> CvResult<Option<[f64; 3]>> {
    let mut bins = [[0; 256]; 3];
    let mut count = 0;
    for (p, &selected) in lab.data_typed::<Vec3b>()?.iter().zip(core.data_bytes()?) {
        if selected == 0 {
            continue;
        }
        count += 1;
        for c in 0..3 {
            bins[c][usize::from(p[c])] += 1;
        }
    }
    Ok((count != 0).then(|| bins.map(|hist| median_bin(&hist, count))))
}

pub(super) fn median_colour_roi(
    lab: &Mat,
    lab_roi: Rect,
    core: &Mat,
    core_roi: Rect,
) -> CvResult<Option<[f64; 3]>> {
    if !lab.is_continuous() || !core.is_continuous() {
        return median_colour(
            &lab.roi(lab_roi)?.try_clone()?,
            &core.roi(core_roi)?.try_clone()?,
        );
    }
    let lab_cols = lab.cols() as usize;
    let core_cols = core.cols() as usize;
    let lab_data = lab.data_typed::<Vec3b>()?;
    let core_data = core.data_bytes()?;
    let w = lab_roi.width as usize;
    let h = lab_roi.height as usize;
    let mut bins = [[0; 256]; 3];
    let mut count = 0;
    for dy in 0..h {
        let lab_start = (lab_roi.y as usize + dy) * lab_cols + lab_roi.x as usize;
        let core_start = (core_roi.y as usize + dy) * core_cols + core_roi.x as usize;
        for (p, &selected) in lab_data[lab_start..lab_start + w]
            .iter()
            .zip(&core_data[core_start..core_start + w])
        {
            if selected == 0 {
                continue;
            }
            count += 1;
            for c in 0..3 {
                bins[c][usize::from(p[c])] += 1;
            }
        }
    }
    Ok((count != 0).then(|| bins.map(|hist| median_bin(&hist, count))))
}

pub(super) fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

pub(super) fn sample_head(lab: &Mat, head: &Contour, b: Rect) -> CvResult<Option<Material>> {
    let mask = filled(head, b)?;
    let core = erode(&mask)?;
    let Some(colour) = median_colour_roi(lab, b, &core, Rect::new(0, 0, b.width, b.height))?
    else {
        return Ok(None);
    };
    let (w, h) = (b.width as usize, b.height as usize);
    let mut chroma = Vec::with_capacity(w * h);
    let core_data = core.data_bytes()?;
    let owned;
    let (lab_data, lab_cols, ox, oy) = if lab.is_continuous() {
        (lab.data_typed::<Vec3b>()?, lab.cols() as usize, b.x as usize, b.y as usize)
    } else {
        owned = lab.roi(b)?.try_clone()?;
        (owned.data_typed::<Vec3b>()?, w, 0, 0)
    };
    for dy in 0..h {
        let lab_start = (oy + dy) * lab_cols + ox;
        let core_start = dy * w;
        for (p, &selected) in lab_data[lab_start..lab_start + w]
            .iter()
            .zip(&core_data[core_start..core_start + w])
        {
            if selected != 0 {
                chroma.push(
                    (f64::from(p[1]) - colour[1]).hypot(f64::from(p[2]) - colour[2]) as f32,
                );
            }
        }
    }
    let rank = (chroma.len() - 1) as f64 * 0.8;
    let (_, lo, upper) = chroma.select_nth_unstable_by(rank as usize, f32::total_cmp);
    let lo = *lo;
    let hi = if rank.fract() == 0. {
        lo
    } else {
        upper.iter().copied().fold(f32::INFINITY, f32::min)
    };
    let spread = f64::from(lo) + rank.fract() * f64::from(hi - lo);
    Ok(Some(Material {
        mask,
        core,
        colour,
        tolerance: (spread + 6.).clamp(12., 20.),
    }))
}

fn material_plane_roi(lab: &Mat, roi: Rect, sample: &Material) -> CvResult<Mat> {
    let mut plane = Mat::zeros(roi.height, roi.width, core::CV_8U)?.to_mat()?;
    let (w, h) = (roi.width as usize, roi.height as usize);
    let owned;
    let (lab_data, lab_cols, ox, oy) = if lab.is_continuous() {
        (lab.data_typed::<Vec3b>()?, lab.cols() as usize, roi.x as usize, roi.y as usize)
    } else {
        owned = lab.roi(roi)?.try_clone()?;
        (owned.data_typed::<Vec3b>()?, w, 0, 0)
    };
    let dl2: [f64; 256] = std::array::from_fn(|v| {
        let d = f64::from(v as u8) - sample.colour[0];
        d * d
    });
    let da2: [f64; 256] = std::array::from_fn(|v| {
        let d = f64::from(v as u8) - sample.colour[1];
        d * d
    });
    let db2: [f64; 256] = std::array::from_fn(|v| {
        let d = f64::from(v as u8) - sample.colour[2];
        d * d
    });
    let tol_f32 = sample.tolerance as f32;
    let tol_lo = f64::from(tol_f32.next_down());
    let tol_hi = f64::from(tol_f32.next_up());
    let tol_lo2 = tol_lo * tol_lo;
    let tol_hi2 = tol_hi * tol_hi;
    let plane_data = plane.data_bytes_mut()?;
    for dy in 0..h {
        let lab_start = (oy + dy) * lab_cols + ox;
        let dst_start = dy * w;
        for (&p, dst) in lab_data[lab_start..lab_start + w]
            .iter()
            .zip(&mut plane_data[dst_start..dst_start + w])
        {
            let l2 = dl2[usize::from(p[0])];
            if l2 > 35. * 35. {
                continue;
            }
            let a2 = da2[usize::from(p[1])];
            if a2 > tol_hi2 {
                continue;
            }
            let chroma2 = a2 + db2[usize::from(p[2])];
            if l2 + chroma2 > 35. * 35. || chroma2 > tol_hi2 {
                continue;
            }
            if chroma2 <= tol_lo2 || f64::from((chroma2 as f32).sqrt()) <= sample.tolerance {
                *dst = 255;
            }
        }
    }
    Ok(plane)
}

fn morph(src: &impl ToInputArray, op: i32, width: i32) -> CvResult<Mat> {
    let mut out = Mat::default();
    let kernel =
        imgproc::get_structuring_element_def(imgproc::MORPH_RECT, Size::new(width, width))?;
    imgproc::morphology_ex_def(src, &mut out, op, &kernel)?;
    Ok(out)
}

fn inside(b: Rect, size: Size) -> bool {
    b.x > 0 && b.y > 0 && b.br().x < size.width && b.br().y < size.height
}

fn and(a: &impl ToInputArray, b: &impl ToInputArray) -> CvResult<Mat> {
    let mut out = Mat::default();
    core::bitwise_and_def(a, b, &mut out)?;
    Ok(out)
}

fn compare(src: &impl ToInputArray, threshold: f64, cmp: i32) -> CvResult<Mat> {
    let mut out = Mat::default();
    core::compare(src, &Scalar::all(threshold), &mut out, cmp)?;
    Ok(out)
}

pub(super) fn recover_heads(lab: &Mat, fragment: &Contour, b: Rect) -> CvResult<Vec<Contour>> {
    let Some(sample) = sample_head(lab, fragment, b)? else {
        return Ok(Vec::new());
    };
    let pad = (f64::from(b.width.max(b.height)) * 0.4).ceil() as i32;
    let roi = Rect::new(b.x - pad, b.y - pad, b.width + 2 * pad, b.height + 2 * pad)
        & Rect::new(0, 0, lab.cols(), lab.rows());
    let plane = morph(
        &material_plane_roi(lab, roi, &sample)?,
        imgproc::MORPH_CLOSE,
        3,
    )?;
    let core_total = core::count_non_zero(&sample.core)?;
    let core_bytes = sample.core.data_bytes()?;
    let (roi_w, sub_x, sub_y, b_w, b_h) = (
        roi.width as usize,
        (b.x - roi.x) as usize,
        (b.y - roi.y) as usize,
        b.width as usize,
        b.height as usize,
    );
    let mut heads = Vec::new();
    for c in contours(&plane, imgproc::RETR_EXTERNAL, roi.tl())? {
        if !inside(imgproc::bounding_rect(&c)? - roi.tl(), roi.size()) {
            continue;
        }
        let component = filled(&c, roi)?;
        let comp_bytes = component.data_bytes()?;
        let mut hits = 0usize;
        for dy in 0..b_h {
            let comp_start = (sub_y + dy) * roi_w + sub_x;
            let core_start = dy * b_w;
            hits += comp_bytes[comp_start..comp_start + b_w]
                .iter()
                .zip(&core_bytes[core_start..core_start + b_w])
                .filter(|(&a, &b)| a != 0 && b != 0)
                .count();
        }
        if (hits as f64) >= 0.6 * f64::from(core_total) {
            heads.push(c);
        }
    }
    Ok(heads)
}

// Repair each component independently. Padding prevents a close from filling
// a neighbouring seam and moving the apparent foot down the platform.
fn repaired_contours(plane: &Mat, width: i32, origin: Point) -> CvResult<Vec<Contour>> {
    let (mut labels, mut stats, mut centres) = (Mat::default(), Mat::default(), Mat::default());
    let n =
        imgproc::connected_components_with_stats_def(plane, &mut labels, &mut stats, &mut centres)?;
    let mut out = Vec::new();
    for i in 1..n {
        let s = stats.at_row::<i32>(i)?;
        let b = Rect::new(s[0], s[1], s[2], s[3]);
        if s[4] < 20 || !inside(b, plane.size()?) {
            continue;
        }
        let component = compare(&labels.roi(b)?, f64::from(i), core::CMP_EQ)?;
        let mut padded = Mat::default();
        core::copy_make_border(
            &component,
            &mut padded,
            width,
            width,
            width,
            width,
            core::BORDER_CONSTANT,
            Scalar::all(0.),
        )?;
        let closed = morph(&padded, imgproc::MORPH_CLOSE, width)?;
        let cropped = closed.roi(Rect::new(width, width, b.width, b.height))?;
        out.extend(contours(&cropped, imgproc::RETR_EXTERNAL, b.tl() + origin)?);
    }
    Ok(out)
}

fn channel(src: &impl ToInputArray, i: i32) -> CvResult<Mat> {
    let mut out = Mat::default();
    core::extract_channel(src, &mut out, i)?;
    Ok(out)
}

fn detail(src: &(impl MatTraitConst + ToInputArray)) -> CvResult<Mat> {
    let (mut blurred, mut diff) = (Mat::default(), Mat::default());
    imgproc::gaussian_blur(
        src,
        &mut blurred,
        Size::new(3, 3),
        0.,
        0.,
        core::BORDER_DEFAULT | core::BORDER_ISOLATED,
        core::AlgorithmHint::ALGO_HINT_DEFAULT,
    )?;
    core::absdiff(src, &blurred, &mut diff)?;
    if src.channels() == 1 {
        return Ok(diff);
    }
    let mut out = Mat::zeros(diff.rows(), diff.cols(), core::CV_8U)?.to_mat()?;
    for (dst, p) in out
        .data_bytes_mut()?
        .iter_mut()
        .zip(diff.data_typed::<Vec3b>()?)
    {
        *dst = p[0].max(p[1]).max(p[2]);
    }
    Ok(out)
}

fn substantial(region: &Mat, width: i32) -> CvResult<Mat> {
    morph(
        &morph(
            &morph(region, imgproc::MORPH_CLOSE, 3)?,
            imgproc::MORPH_OPEN,
            width,
        )?,
        imgproc::MORPH_CLOSE,
        width,
    )
}

fn density_region(material: &Mat, flecks: &Mat, coverage: f64, width: i32) -> CvResult<Mat> {
    let (mut binary, mut density) = (Mat::default(), Mat::default());
    flecks.convert_to(&mut binary, core::CV_32F, 1. / 255., 0.)?;
    imgproc::blur_def(&binary, &mut density, Size::new(width, width))?;
    let dense = compare(&density, (coverage * 0.6).clamp(0.25, 0.5), core::CMP_GE)?;
    substantial(&and(material, &dense)?, width)
}

fn texture_planes(
    lab: &Mat,
    head: Rect,
    roi: Rect,
    sample: &Material,
    mat: &Mat,
    width: i32,
    planes: &mut Vec<Mat>,
) -> CvResult<()> {
    let head_lab = lab.roi(head)?;
    let body_lab = lab.roi(roi)?;
    let texture = detail(&channel(&head_lab, 0)?)?;
    let mut bins = [0; 256];
    let core_size = core::count_non_zero(&sample.core)?;
    for (&v, &selected) in texture.data_bytes()?.iter().zip(sample.core.data_bytes()?) {
        if selected != 0 {
            bins[usize::from(v)] += 1;
        }
    }
    let core_bytes = sample.core.data_bytes()?;
    let coverage = |detail: &Mat, threshold: f64| -> CvResult<f64> {
        let hits = detail
            .data_bytes()?
            .iter()
            .zip(core_bytes)
            .filter(|(&d, &c)| c != 0 && f64::from(d) >= threshold)
            .count();
        Ok(hits as f64 / f64::from(core_size))
    };
    let grain = median_bin(&bins, core_size);
    if grain >= 1. {
        let threshold = (grain * 0.5).clamp(2., 4.);
        let flecks = compare(&detail(&channel(&body_lab, 0)?)?, threshold, core::CMP_GE)?;
        planes.push(substantial(&and(mat, &flecks)?, width)?);
        planes.push(density_region(
            mat,
            &flecks,
            coverage(&texture, threshold)?,
            width,
        )?);
    }
    let fine = coverage(&detail(&head_lab)?, 1.)?;
    if fine >= 0.6 {
        let flecks = compare(&detail(&body_lab)?, 1., core::CMP_GE)?;
        planes.push(density_region(mat, &flecks, fine, width)?);
    }
    Ok(())
}

// Interpolation can join the body to a flat platform. Repeated colours at the
// corridor's left and right edges describe that backdrop, even for new skins.
fn without_backdrop_roi(lab: &Mat, roi: Rect, material: &Mat, width: i32) -> CvResult<Mat> {
    let (w, h) = (roi.width as usize, roi.height as usize);
    let owned;
    let (lab_data, lab_cols, ox, oy) = if lab.is_continuous() {
        (lab.data_typed::<Vec3b>()?, lab.cols() as usize, roi.x as usize, roi.y as usize)
    } else {
        owned = lab.roi(roi)?.try_clone()?;
        (owned.data_typed::<Vec3b>()?, w, 0, 0)
    };
    let mut counts = BTreeMap::new();
    for dy in 0..h {
        let row = &lab_data[(oy + dy) * lab_cols + ox..][..w];
        for x in [0, 1, w - 2, w - 1] {
            *counts.entry(row[x].0).or_insert(0) += 1;
        }
    }
    let bg_colors: Vec<[u8; 3]> = counts
        .into_iter()
        .filter_map(|(color, count)| (count >= width).then_some(color))
        .collect();
    let mut remaining = material.try_clone()?;
    if !bg_colors.is_empty() {
        let rem_data = remaining.data_bytes_mut()?;
        for dy in 0..h {
            let lab_row = &lab_data[(oy + dy) * lab_cols + ox..][..w];
            let rem_row = &mut rem_data[dy * w..][..w];
            for (p, dst) in lab_row.iter().zip(rem_row.iter_mut()) {
                if *dst != 0
                    && bg_colors.iter().any(|&[l, a, b]| {
                        p[0].abs_diff(l) <= 2 && p[1].abs_diff(a) <= 2 && p[2].abs_diff(b) <= 2
                    })
                {
                    *dst = 0;
                }
            }
        }
    }
    substantial(&remaining, width)
}

pub(super) fn visit_bodies(
    lab: &Mat,
    head: Rect,
    sample: &Material,
    backdrop: bool,
    mut original_found: bool,
    mut measure: impl FnMut(&Shape) -> CvResult<bool>,
) -> CvResult<()> {
    let d = diameter(head);
    let cx = center_x(head);
    let left = ((cx - d * 0.85).floor() as i32 - 2).max(0);
    let right = ((cx + d * 0.85).ceil() as i32 + 2).min(lab.cols());
    let top = head.br().y;
    let bottom = ((f64::from(top) + d * 4.15).ceil() as i32 + 2).min(lab.rows());
    if top >= bottom || left >= right {
        return Ok(());
    }
    let roi = Rect::new(left, top, right - left, bottom - top);
    let mat = material_plane_roi(lab, roi, sample)?;
    let cap = (round(f64::from(lab.cols()) * 7. / 553.) | 1).max(3);
    let repair = (round(d * 0.20) | 1).clamp(3, cap);
    let width = (round(d * 0.14) | 1).max(3);
    let mut planes = Vec::new();
    let mut material_count = 0;
    if backdrop {
        let edge = |x| mat.roi(Rect::new(x, 0, 2, roi.height));
        if core::count_non_zero(&edge(0)?)? == 0
            && core::count_non_zero(&edge(roi.width - 2)?)? == 0
        {
            return Ok(());
        }
        planes.push(without_backdrop_roi(lab, roi, &mat, width)?);
    } else {
        planes.push(morph(&mat, imgproc::MORPH_CLOSE, 3)?);
        if repair > 3 {
            planes.push(morph(&mat, imgproc::MORPH_CLOSE, repair)?);
        }
        material_count = planes.len();
        texture_planes(lab, head, roi, sample, &mat, width, &mut planes)?;
    }
    let mut visit = |c: Contour| -> CvResult<bool> {
        let b = imgproc::bounding_rect(&c)?;
        if inside(b - roi.tl(), roi.size()) {
            let (_, area) = hull(&c)?;
            if area >= 20. {
                let solidity = imgproc::contour_area_def(&c)? / area;
                return measure(&Shape {
                    bounds: b,
                    contour: c,
                    solidity,
                });
            }
        }
        Ok(false)
    };
    for plane in &planes {
        for c in contours(plane, imgproc::RETR_EXTERNAL, roi.tl())? {
            original_found |= visit(c)?;
        }
    }
    // An accepted original rules out all repairs for this head. Still visit
    // every original above so competing silhouettes keep their usual order.
    if repair > 3 && !original_found {
        for plane in &planes[material_count..] {
            for c in repaired_contours(plane, repair, roi.tl())? {
                visit(c)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_without_backdrop(patch: &Mat, material: &Mat, width: i32) -> CvResult<Mat> {
        let mut counts = BTreeMap::new();
        for row in patch
            .data_typed::<Vec3b>()?
            .chunks_exact(patch.cols() as usize)
        {
            for x in [0, 1, row.len() - 2, row.len() - 1] {
                *counts.entry(row[x].0).or_insert(0) += 1;
            }
        }
        let mut remaining = material.try_clone()?;
        for ([l, a, b], count) in counts {
            if count < width {
                continue;
            }
            let bound = |delta: f64| {
                Scalar::new(
                    f64::from(l) + delta,
                    f64::from(a) + delta,
                    f64::from(b) + delta,
                    0.,
                )
            };
            let mut same = Mat::default();
            core::in_range(patch, &bound(-2.), &bound(2.), &mut same)?;
            remaining.set_to(&Scalar::all(0.), &same)?;
        }
        substantial(&remaining, width)
    }

    #[test]
    fn without_backdrop_roi_matches_opencv_in_range_at_byte_boundaries() -> CvResult<()> {
        let (full_w, full_h) = (28i32, 20i32);
        let roi = Rect::new(4, 3, 20, 14);
        let mut lab = Mat::zeros(full_h, full_w, core::CV_8UC3)?.to_mat()?;
        let lab_pixels = lab.data_typed_mut::<Vec3b>()?;
        for y in 0..full_h as usize {
            for x in 0..full_w as usize {
                lab_pixels[y * full_w as usize + x] = Vec3b::from([120, 128, 128]);
            }
        }
        // Populate ROI edges with boundary values 0, 255, and an interior colour 120.
        for dy in 0..roi.height as usize {
            let y = roi.y as usize + dy;
            let row = &mut lab_pixels[y * full_w as usize..];
            let edge_color = if dy < 5 {
                [0, 255, 128]
            } else if dy < 10 {
                [255, 0, 128]
            } else {
                [120, 128, 128]
            };
            for &dx in &[0usize, 1, roi.width as usize - 2, roi.width as usize - 1] {
                row[roi.x as usize + dx] = Vec3b::from(edge_color);
            }
            // Place interior test pixels at exact +/-2 and +/-3 boundaries.
            if dy < 10 {
                row[roi.x as usize + 6] = Vec3b::from([2, 253, 130]);
                row[roi.x as usize + 7] = Vec3b::from([3, 253, 130]);
                row[roi.x as usize + 8] = Vec3b::from([253, 2, 126]);
                row[roi.x as usize + 9] = Vec3b::from([252, 2, 126]);
                row[roi.x as usize + 10] = Vec3b::from([60, 60, 60]);
            }
        }
        let mut material = Mat::zeros(roi.height, roi.width, core::CV_8U)?.to_mat()?;
        material.data_bytes_mut()?.fill(255);
        let patch = lab.roi(roi)?.try_clone()?;
        let expected = reference_without_backdrop(&patch, &material, 3)?;
        let actual = without_backdrop_roi(&lab, roi, &material, 3)?;
        assert_eq!(actual.data_bytes()?, expected.data_bytes()?);
        Ok(())
    }

    #[test]
    fn material_plane_roi_lut_matches_direct_formula() -> CvResult<()> {
        let (full_w, full_h) = (24i32, 16i32);
        let roi = Rect::new(3, 2, 18, 12);
        let mut lab = Mat::zeros(full_h, full_w, core::CV_8UC3)?.to_mat()?;
        let data = lab.data_typed_mut::<Vec3b>()?;
        let colour = [120.4, 130.7, 145.2];
        let tolerance = 14.5;
        for (idx, p) in data.iter_mut().enumerate() {
            let l = ((idx * 17) % 256) as u8;
            let a = (110 + ((idx * 7) % 45)) as u8;
            let b = (125 + ((idx * 11) % 45)) as u8;
            *p = Vec3b::from([l, a, b]);
        }
        // Specific boundary pixels in ROI: l2 > 35^2, exact 35^2 border, a2 > tol_hi2, and chroma ULP band.
        let r0 = roi.y as usize * full_w as usize + roi.x as usize;
        data[r0] = Vec3b::from([120, 131, 145]);
        data[r0 + 1] = Vec3b::from([156, 131, 145]);
        data[r0 + 2] = Vec3b::from([85, 131, 145]);
        data[r0 + 3] = Vec3b::from([120, 145, 145]);
        data[r0 + 4] = Vec3b::from([120, 146, 145]);
        data[r0 + 5] = Vec3b::from([120, 145, 148]);
        data[r0 + 6] = Vec3b::from([120, 144, 151]);
        data[r0 + 7] = Vec3b::from([120, 116, 145]);
        let sample = Material {
            mask: Mat::default(),
            core: Mat::default(),
            colour,
            tolerance,
        };
        let actual = material_plane_roi(&lab, roi, &sample)?;
        let actual_bytes = actual.data_bytes()?;
        let data = lab.data_typed::<Vec3b>()?;
        let mut expected = Vec::with_capacity((roi.width * roi.height) as usize);
        for dy in 0..roi.height as usize {
            for dx in 0..roi.width as usize {
                let p = data[(roi.y as usize + dy) * full_w as usize + (roi.x as usize + dx)];
                let [l, a, b] = std::array::from_fn(|i| f64::from(p[i]) - colour[i]);
                let chroma2 = a * a + b * b;
                expected.push(
                    255 * u8::from(
                        l * l + chroma2 <= 35. * 35.
                            && f64::from((chroma2 as f32).sqrt()) <= tolerance,
                    ),
                );
            }
        }
        assert_eq!(actual_bytes, expected.as_slice());
        Ok(())
    }
}
