use crate::{
    vision::{segments, Block, Detection, Piece},
    Config, Frame,
};

pub(crate) fn foot(f: &Frame, d: &Detection) -> bool {
    let Some(p) = &d.piece else {
        return false;
    };
    if f.width == 0 || f.width.checked_mul(f.height) != Some(d.block_mask.len()) {
        return false;
    }
    let x = p.x.round_ties_even() as isize;
    let y = p.y.round_ties_even() as isize;
    let l = (x - 4).clamp(0, f.width as isize) as usize;
    let r = (x + 5).clamp(0, f.width as isize) as usize;
    [6, 10, 14, 18, 24]
        .into_iter()
        .filter(|dy| {
            let row = y + dy;
            if !(0..f.height as isize).contains(&row) {
                return false;
            }
            let offset = row as usize * f.width;
            d.block_mask[offset + l..offset + r]
                .iter()
                .any(|&pixel| pixel != 0)
        })
        .take(2)
        .count()
        == 2
}

fn median(values: &mut [f32]) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    (values[(values.len() - 1) / 2] + values[values.len() / 2]) * 0.5
}

fn sample_points(f: &Frame, mask: &[u8], p: &Piece) -> Vec<(usize, usize)> {
    let (x, y) = (
        p.x.round_ties_even() as isize,
        p.y.round_ties_even() as isize,
    );
    let mut samples = Vec::with_capacity(24);
    for dy in [4, 8, 12, 16, 20, 26] {
        let yy = y + dy;
        if !(0..f.height as isize).contains(&yy) {
            continue;
        }
        for dx in [-6, -2, 2, 6] {
            let xx = x + dx;
            if (0..f.width as isize).contains(&xx) && mask[yy as usize * f.width + xx as usize] != 0
            {
                samples.push((xx as usize, yy as usize));
            }
        }
    }
    samples
}

// For the three brightest patches take the channel-wise median of their
// patch medians. Stable sorting preserves the source sample order on ties.
fn reference_colour(f: &Frame, points: Vec<(usize, usize)>) -> [f32; 3] {
    let mut patches = Vec::with_capacity(points.len());
    for (x, y) in points {
        let mut channels = [[0.0f32; 9]; 3];
        let mut n = 0;
        let mut total = 0.0;
        for yy in y.saturating_sub(1)..=(y + 1).min(f.height - 1) {
            for xx in x.saturating_sub(1)..=(x + 1).min(f.width - 1) {
                let index = (yy * f.width + xx) * 3;
                for c in 0..3 {
                    channels[c][n] = f.bgr[index + c] as f32;
                    total += channels[c][n];
                }
                n += 1;
            }
        }
        patches.push((
            total / (3 * n) as f32,
            channels.map(|mut channel| median(&mut channel[..n])),
        ));
    }
    patches.sort_by(|a, b| b.0.total_cmp(&a.0));
    let n = patches.len().min(3);
    std::array::from_fn(|c| {
        let mut values = [0.0; 3];
        for (v, patch) in values.iter_mut().zip(&patches[..n]) {
            *v = patch.1[c];
        }
        median(&mut values[..n])
    })
}

fn widest(
    f: &Frame,
    mask: &[u8],
    p: &Piece,
    half: f64,
    colour: Option<([f32; 3], f64)>,
) -> Option<(f64, f64)> {
    let mut best = None;
    let mut filtered = Vec::new();
    for dy in (2..34).step_by(2) {
        let y = (p.y + dy as f64).round_ties_even() as isize;
        if !(0..f.height as isize).contains(&y) {
            break;
        }
        let offset = y as usize * f.width;
        let row = &mask[offset..offset + f.width];
        let row = if let Some((reference, tol)) = colour {
            let pixels = &f.bgr[offset * 3..(offset + f.width) * 3];
            filtered.clear();
            filtered.extend(
                pixels
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .zip(row)
                    .map(|(bgr, &value)| {
                        let diff: f32 = (0..3).map(|c| (bgr[c] as f32 - reference[c]).abs()).sum();
                        if diff as f64 <= tol {
                            value
                        } else {
                            0
                        }
                    }),
            );
            &filtered[..]
        } else {
            row
        };
        for (l, r) in segments(row) {
            let width = (r - l + 1) as f64;
            let (l, r) = (l as f64, r as f64);
            if width >= 20.0
                && (l - 12.0..=r + 12.0).contains(&p.x)
                && l >= p.x - half
                && r <= p.x + half
                && best.is_none_or(|(w, _)| width > w)
            {
                best = Some((width, (l + r) * 0.5));
            }
        }
    }
    best
}

pub(crate) fn error(
    before: &Frame,
    after: &Frame,
    d: &Detection,
    t: &Block,
    c: &Config,
    axis: (f64, f64),
) -> Option<(f64, f64, &'static str)> {
    let p = d.piece.as_ref()?;
    if !after.valid() {
        return None;
    }
    let norm = axis.0.hypot(axis.1);
    if c.s("landing_method") == "flow" {
        if norm < 1e-6 || !before.valid() {
            return None;
        }
        let (dx, dy) = before.shift(after, c.f("flow_min_response"))?;
        let anchor_dy = c.f("piece_anchor_dy_ratio") * after.height as f64;
        let ex = p.x - (t.x + dx);
        let ey = p.y - (t.y + dy - anchor_dy);
        let err = ex * (axis.0 / norm) + ey * (axis.1 / norm);
        return Some((err, (t.bbox[2] as f64).max(40.0), "flow"));
    }
    if d.block_mask.len() != after.width * after.height {
        return None;
    }
    let ux = axis.0 / norm.max(1e-6);
    if ux.abs() < 0.5 {
        return None;
    }
    let points = sample_points(after, &d.block_mask, p);
    if points.is_empty() {
        return None;
    }
    let half = if t.top_width > 40.0 {
        (0.62 * t.top_width).max(0.25 * after.width as f64)
    } else {
        0.45 * after.width as f64
    };
    let (width, center) = widest(after, &d.block_mask, p, half, None).or_else(|| {
        if c.s("landing_method") == "block" {
            return None;
        }
        let reference = reference_colour(after, points);
        widest(
            after,
            &d.block_mask,
            p,
            half,
            Some((reference, c.f("landing_color_tol"))),
        )
    })?;
    Some(((p.x - center) / ux, width, "mask"))
}
