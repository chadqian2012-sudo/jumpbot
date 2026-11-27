use crate::regions::{Mask, RegionScratch, Regions};
use crate::{Config, Frame};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Piece {
    pub x: f64,
    pub y: f64,
    pub bbox: [i32; 4],
    pub area: i32,
    pub tpl_score: f64,
    pub source: &'static str,
    pub shape_score: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Block {
    pub x: f64,
    pub y: f64,
    pub bbox: [i32; 4],
    pub area: i32,
    pub top_y: f64,
    pub top_width: f64,
    pub dot: Option<(f64, f64, i32)>,
}

pub struct Detection {
    pub piece: Option<Piece>,
    pub target: Option<Block>,
    pub unresolved_target_shadow: bool,
    pub piece_mask: Vec<u8>,
    pub block_mask: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(crate) struct Aim {
    pub piece: (f64, f64),
    pub target: (f64, f64),
}

impl Detection {
    pub(crate) fn aim(&self) -> Option<Aim> {
        let (piece, target) = self.piece.as_ref().zip(self.target.as_ref())?;
        Some(Aim {
            piece: (piece.x, piece.y),
            target: (target.x, target.y),
        })
    }
}

pub(crate) fn points_close((x, y): (f64, f64), (u, v): (f64, f64), tolerance: f64) -> bool {
    (x - u).abs() <= tolerance && (y - v).abs() <= tolerance
}

impl Aim {
    pub(crate) fn confirmed(self, previous: Option<Self>, cfg: &Config) -> bool {
        previous.is_some_and(|old| {
            points_close(old.piece, self.piece, cfg.f("settle_anchor_tol_px"))
                && points_close(old.target, self.target, cfg.f("target_consistency_px"))
        })
    }
}

fn check_frame(frame: &Frame) -> Result<(), String> {
    let area = frame.width.checked_mul(frame.height);
    if !frame.valid() || area.is_none_or(|n| n > i32::MAX as usize) {
        return Err("Invalid packed BGR frame dimensions".into());
    }
    Ok(())
}

pub fn axis_components(dx: f64, dy: f64, slope: f64) -> ((f64, f64), f64, f64) {
    let norm = (1.0 + slope * slope).sqrt();
    let ux = if dx >= 0.0 { 1.0 / norm } else { -1.0 / norm };
    let uy = -slope / norm;
    ((ux, uy), dx * ux + dy * uy, (dx * -uy + dy * ux).abs())
}

/// Called only after an unknown dark frame has a readable HUD. Require evidence
/// from this frame even when the piece and target match an earlier detection.
pub(crate) fn clear_dark_board(f: &Frame, d: &Detection, cfg: &Config) -> bool {
    let (Some(p), Some(t)) = (&d.piece, &d.target) else {
        return false;
    };
    if !f.valid() || !crate::landing::foot(f, d) {
        return false;
    }
    let (w, h) = (f.width as f64, f.height as f64);
    let [px, py, pw, ph] = p.bbox.map(f64::from);
    let [tx, ty, tw, th] = t.bbox.map(f64::from);
    let trusted_piece = match p.source {
        "legacy" => p.tpl_score >= cfg.f("piece_template_min_score").max(0.52),
        "skin-shape" => p.shape_score >= 0.86,
        _ => false,
    };
    let (_, along, cross) = axis_components(t.x - p.x, t.y - p.y, cfg.f("axis_slope"));
    if !(trusted_piece
        && (0.5 * h..=0.74 * h).contains(&p.y)
        && (0.03 * w..=0.12 * w).contains(&pw)
        && (1.4 * pw..=3.8 * pw).contains(&ph)
        && (0.08 * w..=0.58 * w).contains(&tw)
        && (0.25 * tw..=1.8 * tw).contains(&th)
        && (0.40 * tw..=tw + 1.).contains(&t.top_width)
        && (tx..tx + tw).contains(&t.x)
        && (ty..ty + th).contains(&t.y)
        && t.y < p.y - cfg.f("target_min_up_ratio") * h
        && (cfg.f("min_jump_dist_ratio") * w..=cfg.f("max_jump_dist_ratio") * w).contains(&along)
        && cross <= cfg.f("axis_max_cross_ratio").min(0.30) * along)
    {
        return false;
    }

    // A playfield exposes the same slowly changing colour on both margins and
    // between the two platforms. A fixed panel can leave a real pawn visible
    // underneath and its text can even pass score recognition.
    let pixels = f.bgr.as_chunks::<3>().0;
    let pixel = |x: usize, y: usize| pixels[y * f.width + x];
    let difference = |a: [u8; 3], b: [u8; 3]| (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap();
    let left = f.width * 3 / 100;
    let right = f.width - 1 - left;
    let gap = (p.x + t.x) * 0.5;
    let mut previous = pixel(left, f.height / 4);
    for y in (f.height / 4..(p.y as usize + 10).max(f.height * 65 / 100).min(f.height))
        .step_by((f.height / 160).max(1))
    {
        let bg = pixel(left, y);
        if difference(bg, pixel(right, y)) > 12 || difference(bg, previous) > 8 {
            return false;
        }
        previous = bg;
        for x in (left..=right).step_by((f.width / 160).max(1)) {
            if difference(pixel(x, y), bg) <= 12 {
                continue;
            }
            let (x, y) = (x as f64, y as f64);
            // Allow the pawn's head above the legacy colour mask, its footing,
            // and the target with its leftward shadow. Keep the gap exposed.
            let pawn = (px - 3. ..=px + pw + 3.).contains(&x) && y >= py - 0.55 * ph && y <= p.y;
            let footing = (x - p.x).abs() <= 0.22 * w && y >= p.y - 0.60 * ph;
            let target = (tx - 0.40 * tw..=tx + tw + 3.).contains(&x)
                && (ty - 3. ..=ty + th + 3.).contains(&y);
            if (x - gap).abs() < 0.025 * w || !(pawn || footing || target) {
                return false;
            }
        }
    }
    // Flat/black capture data cannot substantiate even a plausible detection.
    [(p.x, p.y - ph * 0.25), (p.x, p.y + pw * 0.25), (t.x, t.y)]
        .into_iter()
        .all(|(x, y)| {
            let (x, y) = (x as usize, y as usize);
            x < f.width && y < f.height && difference(pixel(x, y), pixel(left, y)) >= 20
        })
}

pub(crate) fn segments(row: &[u8]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut offset = 0;
    std::iter::from_fn(move || {
        let l = offset + row[offset..].iter().position(|v| *v != 0)?;
        offset = row[l..]
            .iter()
            .position(|v| *v == 0)
            .map_or(row.len(), |n| l + n);
        Some((l, offset - 1))
    })
}

static SAT_DIV: std::sync::LazyLock<[i32; 256]> = std::sync::LazyLock::new(|| {
    let mut table = [0; 256];
    for (v, slot) in table.iter_mut().enumerate().skip(1) {
        *slot = ((255 << 12) as f64 / v as f64).round_ties_even() as i32;
    }
    table
});

fn face_center_on_mask(
    mask: &[u8],
    w: usize,
    by: usize,
    bw: usize,
    bh: usize,
    slope: f64,
) -> Option<(f64, f64, f64)> {
    let band = |row: usize| &mask[row * w..][..w];
    let fallback = |corner: f64| {
        let row = ((by as f64 + 0.2887 * bw as f64).round_ties_even() as usize) - by;
        if row >= bh {
            return None;
        }
        // Python's max keeps the first segment of a tied width.
        let (l, r) = segments(band(row)).min_by_key(|(l, r)| std::cmp::Reverse(r - l + 1))?;
        let width = (r - l + 1) as f64;
        if width < 20.0 {
            return None;
        }
        let left = l <= 1;
        let right = r >= w.saturating_sub(2);
        let x = match (left, right) {
            (true, false) => r as f64 - width * 0.5,
            (false, true) => l as f64 + width * 0.5,
            _ => corner,
        };
        Some((x, by as f64 + 0.2887 * width, width))
    };
    let narrow = (0.30 * bw as f64).max(8.0);
    let corners: Vec<_> = segments(band(0))
        .filter(|(l, r)| (r - l + 1) as f64 <= narrow)
        .map(|(l, r)| (l + r) as f64 * 0.5)
        .collect();
    let Some(&first) = corners.first() else {
        let (sum, n) = segments(band(0)).fold((0.0, 0), |(sum, n), (l, r)| {
            let count = r - l + 1;
            (sum + (l + r) as f64 * 0.5 * count as f64, n + count)
        });
        return fallback(sum / n as f64);
    };
    let dx_per_dy = 1.0 / slope.max(1e-6);
    let tolerance = (0.025 * bw as f64).max(3.0);
    let limit = (bh - 1).min((0.6 * bw as f64 * dx_per_dy) as usize + 4);
    let mut best_fit = None;
    for corner in corners {
        if corner <= 10.0 || corner >= w as f64 - 10.0 {
            return fallback(corner);
        }
        // The row range is inclusive and fits in the initialized mask.
        let row = jumpbot_vision::fit_vertex(mask, w, 0, limit, corner, dx_per_dy, tolerance);
        let width = 2.0 * row as f64 * dx_per_dy;
        if row >= 0 && width >= 0.30 * bw as f64 && best_fit.is_none_or(|(_, _, best)| width > best)
        {
            best_fit = Some((corner, by as f64 + row as f64, width));
        }
    }
    if best_fit.is_some() {
        return best_fit;
    }
    let (mut width, mut best_row, mut growth) = (0.0, 0, 0);
    let row_limit = bh.min(((0.45 * bh as f64) as usize).max(8));
    for row in 0..row_limit {
        if width > 0.0 && row - growth >= 8 {
            return Some((first, (by + best_row) as f64, width));
        }
        for (l, r) in segments(band(row))
            .filter(|(l, r)| *l as f64 - 2.0 <= first && first <= *r as f64 + 2.0)
        {
            let span = (r - l + 1) as f64;
            if ((first - l as f64) - (r as f64 - first)).abs() > (0.045 * span).max(3.0) {
                return (width > 0.0).then_some((first, (by + best_row) as f64, width));
            }
            if span > width {
                // Rounded edges can widen one pixel at a time after resampling.
                // Keep the last pronounced expansion, or advance the center when
                // gradual widening continues for a full plateau window.
                if span > width + 1.5 || row - best_row >= 8 {
                    best_row = row;
                }
                growth = row;
                width = span;
            }
        }
    }
    if width <= 0.0 || width < 0.15 * bw as f64 {
        return fallback(first);
    }
    Some((first, (by + best_row) as f64, width))
}

fn face_center_with_scratch(
    w: usize,
    labels: &[i32],
    id: usize,
    stats: [i32; 5],
    slope: f64,
    mask: &mut Vec<u8>,
) -> Option<(f64, f64, f64)> {
    let [bx, by, bw, bh, _] = stats.map(|v| v as usize);
    let needed = w * bh;
    if mask.len() < needed {
        mask.resize(needed, 0);
    }
    // Keep screen x coordinates for the clipped-edge rules, and rebase rows at
    // the component top. Other components must not affect the fitted edges.
    // Invariant: mask is all zeros on entry and cleared back to all zeros on exit.
    for (row, band) in mask[..needed].chunks_exact_mut(w).enumerate() {
        let start = (by + row) * w + bx;
        for (dst, &label) in band[bx..bx + bw]
            .iter_mut()
            .zip(&labels[start..start + bw])
        {
            *dst = u8::from(label == id as i32);
        }
    }
    let result = face_center_on_mask(&mask[..needed], w, by, bw, bh, slope);
    for band in mask[..needed].chunks_exact_mut(w) {
        band[bx..bx + bw].fill(0);
    }
    result
}

pub(crate) fn face_center(
    parts: &Regions,
    id: usize,
    stats: [i32; 5],
    slope: f64,
) -> Option<(f64, f64, f64)> {
    let mut mask = Vec::new();
    face_center_with_scratch(parts.width, &parts.labels, id, stats, slope, &mut mask)
}

struct DotQuery {
    cx: f64,
    cy: f64,
    radius: f64,
    painted: bool,
}

struct TargetScratch<'a> {
    region: &'a mut RegionScratch,
    face_mask: &'a mut Vec<u8>,
    dot_mask: &'a mut Vec<u8>,
}

fn find_dot_if(
    frame: &Frame,
    query: DotQuery,
    cfg: &Config,
    region_scratch: &mut RegionScratch,
    mask: &mut Vec<u8>,
    accept: impl Fn(f64, f64) -> bool,
) -> Result<Option<(f64, f64, i32)>, String> {
    let DotQuery {
        cx,
        cy,
        radius,
        painted,
    } = query;
    let x0 = (cx - radius).round_ties_even().max(0.0) as i32;
    let y0 = (cy - radius).round_ties_even().max(0.0) as i32;
    let x1 = ((cx + radius).round_ties_even() + 1.0).min(frame.width as f64) as i32;
    let y1 = ((cy + radius).round_ties_even() + 1.0).min(frame.height as f64) as i32;
    let bounds = [x0, y0, x1 - x0, y1 - y0];
    if bounds[2] < 6 || bounds[3] < 6 {
        return Ok(None);
    }
    let (w, h) = (bounds[2] as usize, bounds[3] as usize);
    let min_value = cfg.f("center_dot_val_min").ceil() as i32;
    let max_sat = cfg.f("center_dot_sat_max").floor() as i32;
    let min_dot_area = cfg.f("center_dot_min_area");
    let max_dot_area = cfg.f("center_dot_max_area");
    if painted && (min_value > 245 || max_sat < 0) {
        return Ok(None);
    }
    mask.clear();
    mask.reserve(w * h);
    let sat_div = &*SAT_DIV;
    let mut fg_count = 0usize;
    if painted {
        // The unblended game marker has the same ink on bright and dark faces.
        for row in y0..y1 {
            let start = (row as usize * frame.width + x0 as usize) * 3;
            for pixel in frame.bgr[start..start + w * 3].as_chunks::<3>().0 {
                let hit = *pixel == [245; 3];
                fg_count += usize::from(hit);
                mask.push(u8::from(hit));
            }
        }
    } else {
        for row in y0..y1 {
            let start = (row as usize * frame.width + x0 as usize) * 3;
            for pixel in frame.bgr[start..start + w * 3].as_chunks::<3>().0 {
                let v = i32::from(*pixel.iter().max().unwrap());
                let d = v - i32::from(*pixel.iter().min().unwrap());
                let saturation = (d * sat_div[v as usize] + 2048) >> 12;
                let hit = v >= min_value && saturation <= max_sat;
                fg_count += usize::from(hit);
                mask.push(u8::from(hit));
            }
        }
    }
    let min_fg = if min_dot_area > 0.0 {
        min_dot_area.ceil() as usize
    } else {
        1
    };
    if fg_count < min_fg {
        return Ok(None);
    }
    let regions = Mask::from_pixels(mask, w, h).regions_with(region_scratch);
    let labels = &regions.labels;
    let mut nearest = None;
    let mut best_distance = f64::INFINITY;
    let bright_delta = cfg.f("center_dot_bright_delta");
    for (id, &[x, y, bw, bh, area]) in regions.stats.iter().enumerate().skip(1) {
        if (area as f64) < min_dot_area
            || area as f64 > max_dot_area
            || (!painted && bw.max(bh) as f64 > 2.0 * bw.min(bh) as f64)
        {
            continue;
        }
        if painted {
            let (rx, ry) = (f64::from(bw) / 2., f64::from(bh) / 2.);
            let (mx, my) = (f64::from(x) + rx - 0.5, f64::from(y) + ry - 0.5);
            let (dx, dy) = (mx + f64::from(x0), my + f64::from(y0));
            // A projected disc must be complete, near the face estimate, and filled
            // like an ellipse. Allow one raster pixel at the antialiased boundary.
            if x == 0
                || y == 0
                || x + bw == bounds[2]
                || y + bh == bounds[3]
                || (ry - rx * cfg.f("axis_slope")).abs() > 1.
                || (dx - cx).hypot(dy - cy) > radius.min(f64::from(bw))
                || !accept(dx, dy)
            {
                continue;
            }
            let ellipse = (y..y + bh).all(|yy| {
                let dy_n = (f64::from(yy) - my) / ry;
                (x..x + bw).all(|xx| {
                    let radial = ((f64::from(xx) - mx) / rx).hypot(dy_n);
                    let filled = labels[yy as usize * w + xx as usize] == id as i32;
                    filled == (radial <= 1.) || (radial - 1.).abs() * ry <= 1.
                })
            });
            if ellipse {
                if nearest.is_some() {
                    return Ok(None);
                }
                nearest = Some((dx, dy, area));
            }
            continue;
        }
        let (mut sx, mut sy, mut brightness) = (0i64, 0i64, 0i64);
        for yy in y..y + bh {
            let label_row = &labels[yy as usize * w..][..w];
            let frame_row_start = (yy + y0) as usize * frame.width + x0 as usize;
            for xx in x..x + bw {
                if label_row[xx as usize] == id as i32 {
                    sx += xx as i64;
                    sy += yy as i64;
                    let idx = (frame_row_start + xx as usize) * 3;
                    brightness += *frame.bgr[idx..idx + 3].iter().max().unwrap() as i64;
                }
            }
        }
        let dx = sx as f64 / area as f64 + x0 as f64;
        let dy = sy as f64 / area as f64 + y0 as f64;
        let dist = (dx - cx).hypot(dy - cy);
        if dist > radius || dist >= best_distance || !accept(dx, dy) {
            continue;
        }
        // A 5x5 dilation is the set of pixels within two rows and columns of
        // this label. Ring samples are clipped to the ROI just like cv::dilate.
        let mut hist = [0u32; 256];
        let mut ring_count = 0usize;
        for yy in (y - 2).max(0)..(y + bh + 2).min(bounds[3]) {
            let label_row = &labels[yy as usize * w..][..w];
            let ry0 = (yy - 2).max(0);
            let ry1 = (yy + 2).min(bounds[3] - 1);
            let frame_row_start = (yy + y0) as usize * frame.width + x0 as usize;
            for xx in (x - 2).max(0)..(x + bw + 2).min(bounds[2]) {
                if label_row[xx as usize] == id as i32 {
                    continue;
                }
                let rx0 = (xx - 2).max(0) as usize;
                let rx1 = (xx + 2).min(bounds[2] - 1) as usize;
                if (ry0..=ry1).any(|ry| labels[ry as usize * w..][rx0..=rx1].contains(&(id as i32)))
                {
                    let idx = (frame_row_start + xx as usize) * 3;
                    let val = *frame.bgr[idx..idx + 3].iter().max().unwrap();
                    hist[val as usize] += 1;
                    ring_count += 1;
                }
            }
        }
        if ring_count == 0 {
            continue;
        }
        let lo_idx = (ring_count - 1) / 2;
        let hi_idx = ring_count / 2;
        let mut acc = 0usize;
        let (mut lo_val, mut hi_val) = (None, None);
        for (val, &cnt) in hist.iter().enumerate() {
            if cnt == 0 {
                continue;
            }
            acc += cnt as usize;
            if lo_val.is_none() && acc > lo_idx {
                lo_val = Some(val as f64);
            }
            if acc > hi_idx {
                hi_val = Some(val as f64);
                break;
            }
        }
        let median = (lo_val.unwrap_or(0.0) + hi_val.unwrap_or(0.0)) * 0.5;
        if brightness as f64 / (area as f64) < median + bright_delta {
            continue;
        }
        nearest = Some((dx, dy, area));
        best_distance = dist;
    }
    Ok(nearest)
}

pub fn find_dot_near(
    frame: &Frame,
    x: f64,
    y: f64,
    radius: f64,
    cfg: &Config,
) -> Result<Option<(f64, f64, i32)>, String> {
    check_frame(frame)?;
    find_dot_if(
        frame,
        DotQuery {
            cx: x,
            cy: y,
            radius,
            painted: false,
        },
        cfg,
        &mut RegionScratch::default(),
        &mut Vec::new(),
        |_, _| true,
    )
}

struct TargetCandidate {
    block: Block,
    strict: bool,
    small: bool,
    degenerate: bool,
    along: f64,
    cross_ratio: f64,
}

enum TargetSelection {
    Found(Block),
    Missing,
    UnresolvedShadow,
}

fn find_target(
    frame: &Frame,
    block_mask: &[u8],
    piece: &Piece,
    piece_mask: &[u8],
    cfg: &Config,
    scratch: &mut TargetScratch<'_>,
) -> Result<TargetSelection, String> {
    let (w, h) = (frame.width as f64, frame.height as f64);
    let parts = Mask::from_pixels(block_mask, frame.width, frame.height).blocks_with(
        Mask::from_pixels(piece_mask, frame.width, frame.height),
        scratch.region,
    );
    let slope = cfg.f("axis_slope");
    let min_area = cfg.f("block_min_area_ratio") * (w * h);
    let max_area = cfg.f("block_max_area_ratio") * (w * h);
    let low_gate = cfg.f("low_ratio_gate");
    let mut candidates = Vec::new();
    for (id, &stats) in parts.stats.iter().enumerate().skip(1) {
        let [bx, by, bw, bh, area] = stats;
        let (width, height, a) = (bw as f64, bh as f64, area as f64);
        if a > max_area {
            continue;
        }
        let strict = a >= min_area
            && width >= cfg.f("block_min_width_ratio") * w
            && height >= cfg.f("block_min_height_ratio") * h;
        let small = a >= cfg.f("block_min_area_ratio") * 0.4 * (w * h)
            && width >= 0.05 * w
            && height >= 0.05 * h;
        if !(strict || small) {
            continue;
        }
        let Some((x, mut y, top_width)) =
            face_center_with_scratch(parts.width, parts.labels, id, stats, slope, scratch.face_mask)
        else {
            continue;
        };
        let top_y = by as f64;
        if cfg.s("aim_center_mode") == "geometric"
            && top_width >= low_gate * width
            && top_width <= width
        {
            let cy = top_y + 0.5 * top_width * slope;
            if (top_y..=top_y + height).contains(&cy) {
                y = cy;
            }
        }
        let (_, along, cross) = axis_components(x - piece.x, y - piece.y, slope);
        candidates.push(TargetCandidate {
            block: Block {
                x,
                y,
                bbox: [bx, by, bw, bh],
                area,
                top_y,
                top_width,
                dot: None,
            },
            strict,
            small,
            degenerate: top_width < (cfg.f("block_degenerate_top_ratio") * width).max(12.0),
            along,
            cross_ratio: cross / along.abs().max(1.0),
        });
    }
    drop(parts);

    let min_dist = cfg.f("min_jump_dist_ratio") * w;
    let max_dist = cfg.f("max_jump_dist_ratio") * w;
    let cross_limit = cfg.f("axis_max_cross_ratio");
    let max_y = piece.y - cfg.f("target_min_up_ratio") * h;
    // Preserve the three Python passes and their stable component-order ties.
    let choose = |tier: u8, min: f64, ratio: f64| {
        candidates
            .iter()
            .filter(|c| {
                let size_ok = match tier {
                    0 => c.strict && !c.degenerate,
                    1 => c.small && !c.degenerate,
                    _ => c.small,
                };
                size_ok
                    && c.block.y < max_y
                    && (min..=max_dist).contains(&c.along)
                    && c.cross_ratio <= ratio
            })
            .min_by(|a, b| {
                a.degenerate
                    .cmp(&b.degenerate)
                    .then(a.along.total_cmp(&b.along))
            })
    };
    let (mut lower, mut ratio) = (min_dist, cross_limit);
    let selected = choose(0, lower, ratio)
        .or_else(|| choose(1, lower, ratio))
        .or_else(|| {
            lower = min_dist * 0.5;
            ratio = 0.60;
            choose(2, lower, ratio)
        });
    let Some(selected) = selected else {
        return Ok(TargetSelection::Missing);
    };
    let mut chosen = selected.block.clone();
    let width = chosen.bbox[2] as f64;
    // A small asymmetric component can be just the detached shadow on the left
    // of a bottle. Compare the shade against the exposed background on two sides.
    let [bx, by, bw, bh] = chosen.bbox.map(|v| v as usize);
    if !selected.strict && chosen.top_width < low_gate * width && bx >= 3 && by >= 3 {
        let pixel = |x, y| frame.bgr.as_chunks::<3>().0[y * frame.width + x];
        let x = bx + bw / 2;
        let above: [u8; 3] = pixel(x, by - 3);
        let shadow = (1..=3).all(|quarter| {
            let y = by + bh * quarter / 4;
            let bg: [u8; 3] = pixel(bx - 3, y);
            let shade: [u8; 3] = pixel(x, y);
            let base = i32::from(*bg.iter().max().unwrap());
            let value = i32::from(*shade.iter().max().unwrap());
            // The blue side of the prop is excluded by the pawn-color mask.
            // It separates this cast shadow from an ordinary dark surface.
            let right = bx + bw;
            piece_mask[y * frame.width + right..y * frame.width + (right + bw / 2).min(frame.width)]
                .iter()
                .any(|&v| v != 0)
                && 2 * value > base
                && f64::from(base - value) > cfg.f("block_dark_delta")
                && (0..3).all(|i| {
                    bg[i].abs_diff(above[i]) <= 8
                        && (i32::from(shade[i]) * base - i32::from(bg[i]) * value).abs() <= 6 * base
                })
        });
        if shadow {
            // Only the adjoining upright component can own the cap. A complete
            // painted disc on its estimated face supplies the measured center.
            let edge = (bx + bw) as f64;
            let mut caps = candidates.iter().filter(|c| {
                let b = &c.block;
                let [left, top, w, h] = b.bbox.map(f64::from);
                !c.degenerate
                    && h > w
                    && left > 0.
                    && left + w < frame.width as f64
                    && b.top_width >= low_gate * w
                    && (left - edge).abs() <= 0.25 * width
                    && (top + h - (by + bh) as f64).abs() <= 0.5 * bh as f64
                    && b.y < chosen.top_y.min(max_y)
                    && (lower..=max_dist).contains(&c.along)
                    && c.cross_ratio <= ratio
            });
            if let Some(cap) = caps.next().filter(|_| caps.next().is_none()) {
                let b = &cap.block;
                let radius = (b.top_width * 0.5).min(cfg.f("center_dot_max_offset_px"));
                // When dot correction is off, use only the symmetric bright top
                // identified by the existing face fit, never the dot reader.
                let cap_y = b.top_y + 0.5 * b.top_width * slope;
                let white = [-0.3, 0.3].into_iter().all(|side| {
                    let x = (b.x + side * b.top_width).round() as usize;
                    let y = cap_y.round() as usize;
                    if x >= frame.width || y >= frame.height {
                        return false;
                    }
                    let rgb: [u8; 3] = pixel(x, y);
                    let value = f64::from(*rgb.iter().max().unwrap());
                    let spread = value - f64::from(*rgb.iter().min().unwrap());
                    value >= cfg.f("block_white_val_min")
                        && 255.0 * spread <= cfg.f("block_white_sat_max") * value
                });
                if !white {
                    return Ok(TargetSelection::UnresolvedShadow);
                }
                if radius <= 0.0 {
                    return Ok(TargetSelection::Found(b.clone()));
                }
                if let Some(dot) = find_dot_if(
                    frame,
                    DotQuery {
                        cx: b.x,
                        cy: b.y,
                        radius,
                        painted: true,
                    },
                    cfg,
                    scratch.region,
                    scratch.dot_mask,
                    |x, y| {
                        let (_, along, cross) = axis_components(x - piece.x, y - piece.y, slope);
                        (b.top_y..=b.top_y + b.top_width * slope).contains(&y)
                            && y < max_y
                            && (lower..=max_dist).contains(&along)
                            && cross <= ratio * along
                    },
                )? {
                    let mut cap = b.clone();
                    cap.x = dot.0;
                    cap.y = dot.1;
                    cap.dot = Some(dot);
                    return Ok(TargetSelection::Found(cap));
                }
            }
            return Ok(TargetSelection::UnresolvedShadow);
        }
    }
    let aspect = cfg.f("item_rescue_min_aspect");
    if aspect > 0.0 && chosen.bbox[3] as f64 > aspect * width {
        let min_alt_area = (3.0 * chosen.area as f64).max(min_area);
        // Python visits strict-size blocks first, then the relaxed-size list.
        let alt = candidates
            .iter()
            .filter(|c| {
                !c.degenerate
                    && c.block.area as f64 >= min_alt_area
                    && (min_dist..=max_dist).contains(&c.along)
                    && c.cross_ratio <= 0.30
            })
            .min_by(|a, b| a.along.total_cmp(&b.along).then(b.strict.cmp(&a.strict)));
        if let Some(alt) = alt {
            return Ok(TargetSelection::Found(alt.block.clone()));
        }
    }
    let mut radius = cfg.f("center_dot_max_offset_px");
    if radius > 0.0
        && chosen.top_width < cfg.f("center_dot_ratio_gate") * width
        && width >= cfg.f("center_dot_min_block_width")
    {
        if chosen.top_width < cfg.f("center_dot_degenerate_ratio") * width {
            radius = radius.max(cfg.f("center_dot_degenerate_radius_px"));
        }
        let tolerance = (w * 0.01).max(3.0);
        let valid = |x: f64, y: f64| {
            if y >= max_y {
                return false;
            }
            if chosen.top_width >= low_gate * width
                && !(chosen.top_y - tolerance..=chosen.top_y + chosen.top_width * slope + tolerance)
                    .contains(&y)
            {
                return false;
            }
            let (_, along, cross) = axis_components(x - piece.x, y - piece.y, slope);
            (lower..=max_dist).contains(&along) && cross / along.abs().max(1.0) <= ratio
        };
        let painted = find_dot_if(
            frame,
            DotQuery {
                cx: chosen.x,
                cy: chosen.y,
                radius,
                painted: true,
            },
            cfg,
            scratch.region,
            scratch.dot_mask,
            valid,
        )?;
        let dot = match painted {
            Some(mark) => Some(mark),
            None => find_dot_if(
                frame,
                DotQuery {
                    cx: chosen.x,
                    cy: chosen.y,
                    radius,
                    painted: false,
                },
                cfg,
                scratch.region,
                scratch.dot_mask,
                valid,
            )?,
        };
        if let Some(dot) = dot {
            if (dot.0 - chosen.x).abs() > 3.0 || (dot.1 - chosen.y).abs() > 3.0 {
                chosen.x = dot.0;
                chosen.y = dot.1;
                chosen.dot = Some(dot);
            }
        }
    }
    Ok(TargetSelection::Found(chosen))
}

fn piece_templates() -> Result<Vec<crate::template::PieceTemplate>, String> {
    use std::{ffi::OsStr, fs, io::ErrorKind};
    let dir = crate::asset_dir().join("templates");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let png = path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|s| s.eq_ignore_ascii_case("png"));
        if png
            && path
                .file_stem()
                .and_then(OsStr::to_str)
                .is_some_and(|s| s.to_ascii_lowercase().starts_with("piece"))
        {
            paths.push(path);
        }
    }
    paths.sort_by_key(|p| p.as_os_str().to_string_lossy().to_ascii_lowercase());
    let mut templates = Vec::new();
    for path in paths {
        // The source loader skips unreadable PNGs and keeps the other templates.
        if let Ok(template) = Frame::read(&path) {
            check_frame(&template)?;
            templates.push(
                crate::template::PieceTemplate::new(template).map_err(|e| e.to_string())?,
            );
        }
    }
    Ok(templates)
}

fn template_score(
    frame: &Frame,
    bbox: [i32; 4],
    templates: &[crate::template::PieceTemplate],
) -> Result<f64, String> {
    if bbox[2].min(bbox[3]) < 6 {
        return Ok(0.0);
    }
    let mut best: f64 = 0.0;
    for tpl in templates {
        let score = crate::template::score(frame, bbox, tpl).map_err(|e| e.to_string())?;
        best = best.max(score);
    }
    Ok(best)
}

fn template_scan(frame: &Frame, tpl: &Frame, cfg: &Config) -> Result<Option<Piece>, String> {
    let top = (0.30 * frame.height as f64) as usize;
    if tpl.height >= frame.height - top || tpl.width >= frame.width {
        return Ok(None);
    }
    let (x, y, score) = crate::template::scan(frame, top, tpl).map_err(|e| e.to_string())?;
    let foot_y = (y + tpl.height as i32 - 1) as f64;
    if score < cfg.f("piece_scan_min_score")
        || foot_y < cfg.f("piece_min_y_ratio") * frame.height as f64
    {
        return Ok(None);
    }
    Ok(Some(Piece {
        x: x as f64 + tpl.width as f64 / 2.0,
        y: foot_y,
        bbox: [x, y, tpl.width as i32, tpl.height as i32],
        area: (tpl.width * tpl.height) as i32,
        tpl_score: score,
        source: "legacy",
        shape_score: 0.0,
    }))
}

fn accept_skin(m: crate::skin::SkinMatch, stride: usize, pm: &mut [u8], bm: &mut [u8]) -> Piece {
    let (x, y, w, h) = m.bbox;
    for (dy, row) in m.mask.chunks_exact(w).enumerate() {
        let start = (y + dy) * stride + x;
        for (dst, &pixel) in pm[start..start + w].iter_mut().zip(row) {
            *dst |= pixel;
        }
    }
    for (px, py) in m.support_points {
        bm[py * stride + px] = 255;
    }
    Piece {
        x: m.x,
        y: m.y,
        bbox: [x as i32, y as i32, w as i32, h as i32],
        area: m.mask.iter().filter(|p| **p != 0).count() as i32,
        tpl_score: 0.0,
        source: "skin-shape",
        shape_score: m.score,
    }
}

pub struct Vision {
    cfg: Config,
    pub masks: crate::masks::Masks,
    templates: Vec<crate::template::PieceTemplate>,
    prior: Option<Piece>,
    region_scratch: RegionScratch,
    face_mask: Vec<u8>,
    dot_mask: Vec<u8>,
    skin_scratch: crate::skin::SkinScratch,
}

impl Vision {
    pub fn new(cfg: &Config, backend: &str) -> Result<Self, String> {
        Ok(Self {
            cfg: cfg.clone(),
            masks: crate::masks::Masks::new(cfg, backend),
            templates: piece_templates()?,
            prior: None,
            region_scratch: RegionScratch::default(),
            face_mask: Vec::new(),
            dot_mask: Vec::new(),
            skin_scratch: crate::skin::SkinScratch::default(),
        })
    }

    pub fn backend(&self) -> &str {
        self.masks.backend()
    }

    pub fn reset_prior(&mut self) {
        self.prior = None;
    }

    pub fn detect(&mut self, frame: &Frame) -> Result<Detection, String> {
        self.detect_frame(frame, true)
    }

    pub fn detect_piece_only(&mut self, frame: &Frame) -> Result<Detection, String> {
        self.detect_frame(frame, false)
    }

    fn detect_frame(&mut self, frame: &Frame, want_target: bool) -> Result<Detection, String> {
        check_frame(frame)?;
        let (mut piece_mask, mut block_mask) = self.masks.compute(frame)?;
        let expected = frame.width * frame.height;
        if piece_mask.len() != expected || block_mask.len() != expected {
            return Err("Mask dimensions differ from the BGR frame".into());
        }
        let mut detection = Detection {
            piece: self.find_piece(frame, &mut piece_mask, &mut block_mask)?,
            target: None,
            unresolved_target_shadow: false,
            piece_mask,
            block_mask,
        };
        if want_target {
            self.complete_target(frame, &mut detection)?;
        }
        if let Some(p) = &detection.piece {
            self.prior = Some(p.clone());
        }
        Ok(detection)
    }

    /// Complete a piece-only detection from this same frame.
    pub(crate) fn complete_target(
        &mut self,
        frame: &Frame,
        d: &mut Detection,
    ) -> Result<(), String> {
        if let Some(p) = &d.piece {
            let mut scratch = TargetScratch {
                region: &mut self.region_scratch,
                face_mask: &mut self.face_mask,
                dot_mask: &mut self.dot_mask,
            };
            let selection = find_target(
                frame,
                &d.block_mask,
                p,
                &d.piece_mask,
                &self.cfg,
                &mut scratch,
            )?;
            d.unresolved_target_shadow = matches!(selection, TargetSelection::UnresolvedShadow);
            d.target = match selection {
                TargetSelection::Found(block) => Some(block),
                _ => None,
            };
        }
        Ok(())
    }

    fn find_piece(
        &mut self,
        frame: &Frame,
        pm: &mut [u8],
        bm: &mut [u8],
    ) -> Result<Option<Piece>, String> {
        let (w, h) = (frame.width as f64, frame.height as f64);
        let cfg = &self.cfg;
        let parts = Mask::from_pixels(pm, frame.width, frame.height).piece();
        let min_area = cfg.f("piece_min_area_ratio") * (w * h);
        let max_area = cfg.f("piece_max_area_ratio") * (w * h);
        let foot_ratio = cfg.f("piece_min_y_ratio");
        let score_min = cfg.f("piece_template_min_score");
        let auto_skin = cfg.b("piece_skin_shape_enabled");
        let mut candidates = Vec::new();
        let mut horizontal = false;
        for &[x, y, bw, bh, area] in &parts.stats[1..] {
            let foot_y = (y + bh) as f64;
            if !(min_area..=max_area).contains(&(area as f64)) || foot_y < foot_ratio * h {
                continue;
            }
            horizontal |= bw as f64 > bh as f64 * 1.25;
            if (bw as f64) < cfg.f("piece_min_width_ratio") * w
                || (bh as f64) < bw as f64 * cfg.f("piece_aspect_min")
                || (y as f64) < cfg.f("top_bar_ratio") * h
                || x <= 1
                || (x + bw) as f64 >= w - 1.0
            {
                continue;
            }
            let bbox = [x, y, bw, bh];
            candidates.push(Piece {
                x: x as f64 + bw as f64 / 2.0,
                y: foot_y,
                bbox,
                area,
                tpl_score: template_score(frame, bbox, &self.templates)?,
                source: "legacy",
                shape_score: 0.0,
            });
        }
        let mut skin = None;
        if auto_skin {
            candidates.retain(|p| p.tpl_score >= score_min);
            if candidates.is_empty() {
                skin = crate::skin::detect_skin_with(
                    frame,
                    Some(bm),
                    foot_ratio,
                    !horizontal,
                    &mut self.skin_scratch,
                );
                if let Some(m) = &skin {
                    let (x, y, bw, bh) = m.bbox;
                    let covered = parts
                        .foreground_in([x as i32, y as i32, bw as i32, bh as i32], Some(&m.mask));
                    let total = m.mask.iter().filter(|p| **p != 0).count().max(1);
                    if (covered as f64) < 0.4 * total as f64 {
                        return Ok(skin.map(|m| accept_skin(m, frame.width, pm, bm)));
                    }
                } else if horizontal {
                    return Ok(None);
                }
            }
        }
        if candidates.is_empty() || !cfg.b("piece_scan_only_when_needed") {
            let top = (0.30 * frame.height as f64) as usize;
            let fg_below_top: usize = if auto_skin {
                parts.stats[1..]
                    .iter()
                    .filter(|s| (s[1] + s[3]) as usize > top)
                    .map(|s| s[4] as usize)
                    .sum()
            } else {
                0
            };
            for tpl in &self.templates {
                let tpl_area = tpl.frame.width * tpl.frame.height;
                if auto_skin && (fg_below_top as f64) < 0.4 * tpl_area as f64 {
                    continue;
                }
                let Some(hit) = template_scan(frame, &tpl.frame, cfg)? else {
                    continue;
                };
                if auto_skin && (parts.foreground_in(hit.bbox, None) as f64) < 0.4 * hit.area as f64
                {
                    continue;
                }
                if let Some(old) = candidates
                    .iter_mut()
                    .find(|p| (p.x - hit.x).hypot(p.y - hit.y) < 0.05 * w)
                {
                    if hit.tpl_score > old.tpl_score {
                        *old = hit;
                    }
                } else {
                    candidates.push(hit);
                }
            }
        }
        if candidates.is_empty() {
            return Ok(skin.map(|m| accept_skin(m, frame.width, pm, bm)));
        }
        if !self.templates.is_empty() && candidates.iter().any(|p| p.tpl_score >= score_min) {
            candidates.retain(|p| p.tpl_score >= score_min);
        }
        if let Some(previous) = &self.prior {
            let distance = |p: &Piece| (p.x - previous.x).hypot(p.y - previous.y);
            if let Some(near) = candidates
                .iter()
                .filter(|p| distance(p) <= cfg.f("piece_prior_radius") * w)
                .min_by(|a, b| {
                    b.tpl_score
                        .total_cmp(&a.tpl_score)
                        .then(distance(a).total_cmp(&distance(b)))
                })
            {
                return Ok(Some(near.clone()));
            }
        }
        Ok(candidates.into_iter().min_by(|a, b| {
            b.tpl_score
                .total_cmp(&a.tpl_score)
                .then(b.area.cmp(&a.area))
        }))
    }
}

#[cfg(test)]
mod dark_board_tests {
    use super::*;
    use crate::screen::{Screen, ScreenState};
    use std::path::Path;

    #[test]
    fn dark_background_still_requires_an_exposed_supported_board() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let cfg = Config::default();
        let screen = Screen::new(&root.join("templates"), &cfg).unwrap();
        let mut vision = Vision::new(&cfg, "cpu").unwrap();
        let source = Frame::read(&root.join("tests/fixtures/skin-gold-narrow-pylon.png")).unwrap();
        for tint in [[104, 99, 95], [74, 83, 95]] {
            let mut f = source.clone();
            // The right margin is clear in this fixture. Preserve the HUD and
            // foreground pixels, recolouring only the matching row background.
            for (y, row) in f
                .bgr
                .chunks_exact_mut(f.width * 3)
                .enumerate()
                .skip(f.height * 6 / 100)
            {
                let pixels = row.as_chunks_mut::<3>().0;
                let edge = pixels[f.width - 5];
                let shade = (18 * y / f.height) as u8;
                for (x, pixel) in pixels.iter_mut().enumerate() {
                    let hud = (f.height * 13 / 100..f.height * 24 / 100).contains(&y)
                        && (f.width * 8 / 100..f.width * 80 / 100).contains(&x);
                    if !hud && (0..3).all(|i| pixel[i].abs_diff(edge[i]) <= 8) {
                        *pixel = tint.map(|channel| channel - shade);
                    }
                }
            }
            assert_eq!(screen.state(&f, &cfg).unwrap(), ScreenState::UnknownDark);
            assert_eq!(screen.score(&f, &cfg), Some(3458));
            vision.reset_prior();
            let mut d = vision.detect(&f).unwrap();
            vision.reset_prior();
            let mut sampled = vision.detect_piece_only(&f).unwrap();
            vision.complete_target(&f, &mut sampled).unwrap();
            assert_eq!(sampled.piece_mask, d.piece_mask);
            assert_eq!(sampled.block_mask, d.block_mask);
            assert_eq!(serde_json::json!(sampled.piece), serde_json::json!(d.piece));
            assert_eq!(
                serde_json::json!(sampled.target),
                serde_json::json!(d.target)
            );
            assert_eq!(sampled.unresolved_target_shadow, d.unresolved_target_shadow);
            assert_eq!(screen.dark_board_score(&f, &cfg, &sampled), Some(3458));
            assert!(
                clear_dark_board(&f, &d, &cfg),
                "{tint:?}: {:?} {:?}",
                d.piece,
                d.target
            );

            // A stable overlay in the gap must veto even the previously good pair.
            let mut covered = f.clone();
            for row in covered
                .bgr
                .chunks_exact_mut(f.width * 3)
                .skip(f.height / 2)
                .take(f.height / 12)
            {
                row[f.width * 43 / 100 * 3..f.width * 57 / 100 * 3].fill(38);
            }
            assert!(!clear_dark_board(&covered, &d, &cfg));
            // Preserve both detections, but erase the actual surface under the foot.
            covered = f.clone();
            let pawn = d.piece.as_ref().unwrap();
            for row in covered
                .bgr
                .chunks_exact_mut(f.width * 3)
                .skip(pawn.y as usize + 1)
                .take(pawn.bbox[2] as usize)
            {
                let pixels = row.as_chunks_mut::<3>().0;
                let bg = pixels[f.width * 3 / 100];
                pixels[pawn.bbox[0] as usize..(pawn.bbox[0] + pawn.bbox[2]) as usize].fill(bg);
            }
            assert!(!clear_dark_board(&covered, &d, &cfg));
            covered.bgr.fill(0);
            assert!(!clear_dark_board(&covered, &d, &cfg));
            let black = vision.detect(&covered).unwrap();
            assert!(!clear_dark_board(&covered, &black, &cfg));
            let mut target = d.target.take().unwrap();
            assert!(!clear_dark_board(&f, &d, &cfg));
            let width = target.bbox[2];
            target.bbox[2] = f.width as i32;
            d.target = Some(target);
            assert!(!clear_dark_board(&f, &d, &cfg));
            d.target.as_mut().unwrap().bbox[2] = width;
            d.block_mask.fill(0);
            assert!(!clear_dark_board(&f, &d, &cfg));
        }
    }

    #[test]
    #[ignore = "requires original capture archive in JUMPBOT_CAPTURE_ARCHIVE"]
    fn original_skin_dialog_and_score_cards_never_clear() {
        let root = std::path::PathBuf::from(std::env::var_os("JUMPBOT_CAPTURE_ARCHIVE").unwrap());
        let cfg = Config::default();
        let mut vision = Vision::new(&cfg, "cpu").unwrap();
        for name in [
            "state_now6/probe.png",
            "gameover/probe.png",
            "run_20260930_194646/jump_003_raw.png",
            "run_20261008_214654/jump_379_raw_after.png",
        ] {
            let f = Frame::read(&root.join(name)).unwrap();
            vision.reset_prior();
            let d = vision.detect(&f).unwrap();
            if name == "state_now6/probe.png" {
                assert!(
                    d.piece.is_some() && d.target.is_some(),
                    "dialog exposes both"
                );
            }
            assert!(!clear_dark_board(&f, &d, &cfg), "{name}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottle_shadow_requires_a_supported_top() {
        let images = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/bottle-shadow");
        for (name, x, y) in [
            ("development.png", 400usize, 501usize),
            ("holdout.png", 393, 505),
        ] {
            let frame = Frame::read(&images.join(name)).unwrap();
            for dots in [true, false] {
                let mut cfg = Config::default();
                if !dots {
                    cfg.0["center_dot_max_offset_px"] = 0.into();
                }
                let mut vision = Vision::new(&cfg, "cpu").unwrap();
                let seen = vision.detect(&frame).unwrap();
                let cap = seen.target.unwrap();
                assert!(!seen.unresolved_target_shadow && cap.dot.is_some() == dots);
                assert!((cap.x - x as f64).hypot(cap.y - y as f64) <= 2.);

                // Keep the bottle and its cast shadow, covering the marker when
                // enabled, or the independently checked bright top when disabled.
                let mut covered = frame.clone();
                let tint = if dots { [255; 3] } else { [160, 180, 200] };
                for yy in y - 7..=y + 7 {
                    for xx in x - 19..=x + 19 {
                        covered.bgr.as_chunks_mut::<3>().0[yy * frame.width + xx] = tint;
                    }
                }
                vision.reset_prior();
                let rejected = vision.detect(&covered).unwrap();
                assert!(
                    rejected.piece.is_some()
                        && rejected.target.is_none()
                        && rejected.unresolved_target_shadow,
                    "{name}, dots={dots}"
                );
                covered.bgr.fill(200);
                let [px, py, w, h] = seen.piece.unwrap().bbox.map(|v| v as usize);
                for row in py..py + h {
                    let start = (row * frame.width + px) * 3;
                    covered.bgr[start..start + 3 * w]
                        .copy_from_slice(&frame.bgr[start..start + 3 * w]);
                }
                vision.reset_prior();
                let missing = vision.detect(&covered).unwrap();
                assert!(
                    missing.piece.is_some()
                        && missing.target.is_none()
                        && !missing.unresolved_target_shadow
                );
            }
        }
    }

    #[test]
    fn sat_div_table_matches_f64_formula_for_all_bytes() {
        for (v, &actual) in SAT_DIV.iter().enumerate() {
            let expected = if v == 0 {
                0
            } else {
                ((255 << 12) as f64 / v as f64).round_ties_even() as i32
            };
            assert_eq!(actual, expected, "v={v}");
        }
    }

    #[test]
    fn face_center_with_scratch_preserves_both_centers_and_resets_buffer() {
        let (w, h) = (224, 120);
        let mut pixels = vec![0u8; w * h];
        // Draw two diamonds in different components.
        for &(cx, cy, half_h) in &[(40isize, 20isize, 22isize), (145isize, 10isize, 35isize)] {
            for dy in 0..=2 * half_h {
                let span = if dy <= half_h { dy } else { 2 * half_h - dy };
                let dx_max = (span as f64 / 0.5774).round() as isize;
                let y = (cy + dy) as usize;
                for x in (cx - dx_max)..=(cx + dx_max) {
                    pixels[y * w + x as usize] = 255;
                }
            }
        }
        let regions = Mask::from_pixels(&pixels, w, h).regions();
        let diamonds = [(145., 45., 35.), (40., 42., 22.)];
        assert_eq!(regions.stats.len() - 1, diamonds.len());
        let mut scratch = Vec::new();
        for (i, (x, y, half_height)) in diamonds.into_iter().enumerate() {
            let id = i + 1;
            let actual = face_center_with_scratch(
                regions.width, &regions.labels, id, regions.stats[id], 0.5774, &mut scratch,
            );
            let width = 2. * half_height * (1. / 0.5774);
            assert_eq!(actual, Some((x, y, width)), "component {id}");
            assert!(
                scratch.iter().all(|&b| b == 0),
                "scratch not cleared after component {id}"
            );
        }
    }
}
