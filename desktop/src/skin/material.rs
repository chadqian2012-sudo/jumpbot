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

pub(super) fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

pub(super) fn sample_head(lab: &Mat, head: &Contour, b: Rect) -> CvResult<Option<Material>> {
    let mask = filled(head, b)?;
    let core = erode(&mask)?;
    let patch = lab.roi(b)?.try_clone()?;
    let Some(colour) = median_colour(&patch, &core)? else {
        return Ok(None);
    };
    let mut chroma = Vec::new();
    for (p, &selected) in patch.data_typed::<Vec3b>()?.iter().zip(core.data_bytes()?) {
        if selected != 0 {
            chroma.push((f64::from(p[1]) - colour[1]).hypot(f64::from(p[2]) - colour[2]) as f32);
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

fn material_plane(lab: &impl MatTraitConst, sample: &Material) -> CvResult<Mat> {
    let mut plane = Mat::zeros(lab.rows(), lab.cols(), core::CV_8U)?.to_mat()?;
    for (&p, dst) in lab
        .data_typed::<Vec3b>()?
        .iter()
        .zip(plane.data_bytes_mut()?)
    {
        let [l, a, b] = std::array::from_fn(|i| f64::from(p[i]) - sample.colour[i]);
        let chroma2 = a * a + b * b;
        *dst = 255
            * u8::from(
                l * l + chroma2 <= 35. * 35.
                    && f64::from((chroma2 as f32).sqrt()) <= sample.tolerance,
            );
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
        &material_plane(&lab.roi(roi)?.try_clone()?, &sample)?,
        imgproc::MORPH_CLOSE,
        3,
    )?;
    let mut heads = Vec::new();
    for c in contours(&plane, imgproc::RETR_EXTERNAL, roi.tl())? {
        if !inside(imgproc::bounding_rect(&c)? - roi.tl(), roi.size()) {
            continue;
        }
        let component = filled(&c, roi)?;
        let hits = core::count_non_zero(&and(&component.roi(b - roi.tl())?, &sample.core)?)?;
        if f64::from(hits) >= 0.6 * f64::from(core::count_non_zero(&sample.core)?) {
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
    let coverage = |detail: &Mat, threshold: f64| -> CvResult<f64> {
        Ok(f64::from(core::count_non_zero(&and(
            &compare(detail, threshold, core::CMP_GE)?,
            &sample.core,
        )?)?)
            / f64::from(core_size))
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
fn without_backdrop(
    patch: &(impl MatTraitConst + ToInputArray),
    material: &Mat,
    width: i32,
) -> CvResult<Mat> {
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
    let patch = lab.roi(roi)?.try_clone()?;
    let mat = material_plane(&patch, sample)?;
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
        planes.push(without_backdrop(&patch, &mat, width)?);
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
