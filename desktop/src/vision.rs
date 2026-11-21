use crate::{Config, Frame};
use serde::Serialize;
use std::{ffi::c_void, mem::MaybeUninit, slice};

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
    pub piece_mask: Vec<u8>,
    pub block_mask: Vec<u8>,
}

#[repr(C)]
struct NativeRegions {
    labels: *const i32,
    stats: *const [i32; 5],
    count: i32,
    owner: *mut c_void,
}

extern "C" {
    fn jb_regions_free(owner: *mut c_void);
    fn jb_piece_regions(pixels: *const u8, w: i32, h: i32, out: *mut NativeRegions) -> i32;
    fn jb_block_regions(
        pixels: *const u8,
        piece: *const u8,
        w: i32,
        h: i32,
        out: *mut NativeRegions,
    ) -> i32;
    fn jb_dot_regions(
        pixels: *const u8,
        w: i32,
        h: i32,
        bounds: *const i32,
        min_value: i32,
        max_sat: i32,
        out: *mut NativeRegions,
    ) -> i32;
    fn jb_piece_score(
        pixels: *const u8,
        w: i32,
        h: i32,
        bounds: *const i32,
        tpl: *const u8,
        tw: i32,
        th: i32,
        score: *mut f64,
    ) -> i32;
    fn jb_piece_scan(
        pixels: *const u8,
        w: i32,
        h: i32,
        top: i32,
        tpl: *const u8,
        tw: i32,
        th: i32,
        x: *mut i32,
        y: *mut i32,
        score: *mut f64,
    ) -> i32;
}

struct Regions {
    raw: NativeRegions,
    width: usize,
    height: usize,
}

impl Drop for Regions {
    fn drop(&mut self) {
        unsafe { jb_regions_free(self.raw.owner) };
    }
}

impl Regions {
    fn new(
        width: usize,
        height: usize,
        operation: &str,
        label: impl FnOnce(*mut NativeRegions) -> i32,
    ) -> Result<Self, String> {
        let mut raw = MaybeUninit::uninit();
        if label(raw.as_mut_ptr()) != 0 {
            return Err(format!("OpenCV {operation} failed"));
        }
        // The native labeler owns the two contiguous matrices until Drop.
        Ok(Self {
            raw: unsafe { raw.assume_init() },
            width,
            height,
        })
    }

    fn stats(&self) -> &[[i32; 5]] {
        unsafe { slice::from_raw_parts(self.raw.stats, self.raw.count as usize) }
    }

    fn labels(&self) -> &[i32] {
        unsafe { slice::from_raw_parts(self.raw.labels, self.width * self.height) }
    }

    fn foreground_in(&self, bounds: [i32; 4], silhouette: Option<&[u8]>) -> usize {
        let [x, y, w, h] = bounds.map(|n| n as usize);
        let mut count = 0;
        for row in 0..h {
            let start = (y + row) * self.width + x;
            let labels = &self.labels()[start..start + w];
            count += match silhouette {
                Some(mask) => labels
                    .iter()
                    .zip(&mask[row * w..][..w])
                    .filter(|(label, occupied)| **label != 0 && **occupied != 0)
                    .count(),
                None => labels.iter().filter(|label| **label != 0).count(),
            };
        }
        count
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

fn face_center(parts: &Regions, id: usize, stats: [i32; 5], slope: f64) -> Option<(f64, f64, f64)> {
    let [bx, by, bw, bh, _] = stats.map(|v| v as usize);
    let w = parts.width;
    // Keep screen x coordinates for the clipped-edge rules, and rebase rows at
    // the component top. Other components must not affect the fitted edges.
    let mut mask = vec![0u8; w * bh];
    for (row, band) in mask.chunks_exact_mut(w).enumerate() {
        let start = (by + row) * w + bx;
        for (dst, &label) in band[bx..bx + bw]
            .iter_mut()
            .zip(&parts.labels()[start..start + bw])
        {
            *dst = u8::from(label == id as i32);
        }
    }
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
        let row = jumpbot_vision::fit_vertex(&mask, w, 0, limit, corner, dx_per_dy, tolerance);
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

fn find_dot_if(
    frame: &Frame,
    cx: f64,
    cy: f64,
    radius: f64,
    cfg: &Config,
    accept: impl Fn(f64, f64) -> bool,
) -> Result<Option<(f64, f64, i32)>, String> {
    let x0 = (cx - radius).round_ties_even().max(0.0) as i32;
    let y0 = (cy - radius).round_ties_even().max(0.0) as i32;
    let x1 = ((cx + radius).round_ties_even() + 1.0).min(frame.width as f64) as i32;
    let y1 = ((cy + radius).round_ties_even() + 1.0).min(frame.height as f64) as i32;
    let bounds = [x0, y0, x1 - x0, y1 - y0];
    if bounds[2] < 6 || bounds[3] < 6 {
        return Ok(None);
    }
    let (w, h) = (bounds[2] as usize, bounds[3] as usize);
    let regions = Regions::new(w, h, "center dot components", |out| unsafe {
        jb_dot_regions(
            frame.bgr.as_ptr(),
            frame.width as i32,
            frame.height as i32,
            bounds.as_ptr(),
            cfg.f("center_dot_val_min").ceil() as i32,
            cfg.f("center_dot_sat_max").floor() as i32,
            out,
        )
    })?;
    let labels = regions.labels();
    let mut nearest = None;
    let mut best_distance = f64::INFINITY;
    for (id, &[x, y, bw, bh, area]) in regions.stats().iter().enumerate().skip(1) {
        if (area as f64) < cfg.f("center_dot_min_area")
            || area as f64 > cfg.f("center_dot_max_area")
            || bw.max(bh) as f64 > 2.0 * bw.min(bh) as f64
        {
            continue;
        }
        let mut ring = Vec::new();
        let (mut sx, mut sy, mut brightness) = (0i64, 0i64, 0i64);
        // A 5x5 dilation is the set of pixels within two rows and columns of
        // this label. Ring samples are clipped to the ROI just like cv::dilate.
        for yy in (y - 2).max(0)..(y + bh + 2).min(bounds[3]) {
            for xx in (x - 2).max(0)..(x + bw + 2).min(bounds[2]) {
                let idx = ((yy + y0) as usize * frame.width + (xx + x0) as usize) * 3;
                let val = *frame.bgr[idx..idx + 3].iter().max().unwrap();
                if labels[yy as usize * w + xx as usize] == id as i32 {
                    sx += xx as i64;
                    sy += yy as i64;
                    brightness += val as i64;
                } else if ((yy - 2).max(0)..=(yy + 2).min(bounds[3] - 1)).any(|ry| {
                    let row = &labels[ry as usize * w..][..w];
                    row[(xx - 2).max(0) as usize..=(xx + 2).min(bounds[2] - 1) as usize]
                        .contains(&(id as i32))
                }) {
                    ring.push(val);
                }
            }
        }
        let dx = sx as f64 / area as f64 + x0 as f64;
        let dy = sy as f64 / area as f64 + y0 as f64;
        let dist = (dx - cx).hypot(dy - cy);
        if dist > radius || dist >= best_distance || !accept(dx, dy) || ring.is_empty() {
            continue;
        }
        ring.sort_unstable();
        let median = (ring[(ring.len() - 1) / 2] as f64 + ring[ring.len() / 2] as f64) * 0.5;
        if brightness as f64 / (area as f64) < median + cfg.f("center_dot_bright_delta") {
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
    find_dot_if(frame, x, y, radius, cfg, |_, _| true)
}

struct TargetCandidate {
    block: Block,
    strict: bool,
    small: bool,
    degenerate: bool,
    along: f64,
    cross_ratio: f64,
}

fn find_target(
    frame: &Frame,
    block_mask: &[u8],
    piece: &Piece,
    piece_mask: &[u8],
    cfg: &Config,
) -> Result<Option<Block>, String> {
    let (w, h) = (frame.width as f64, frame.height as f64);
    let parts = Regions::new(
        frame.width,
        frame.height,
        "block components",
        |out| unsafe {
            jb_block_regions(
                block_mask.as_ptr(),
                piece_mask.as_ptr(),
                frame.width as i32,
                frame.height as i32,
                out,
            )
        },
    )?;
    let slope = cfg.f("axis_slope");
    let min_area = cfg.f("block_min_area_ratio") * (w * h);
    let max_area = cfg.f("block_max_area_ratio") * (w * h);
    let low_gate = cfg.f("low_ratio_gate");
    let mut candidates = Vec::new();
    for (id, &stats) in parts.stats().iter().enumerate().skip(1) {
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
        let Some((x, mut y, top_width)) = face_center(&parts, id, stats, slope) else {
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
        return Ok(None);
    };
    let mut chosen = selected.block.clone();
    let width = chosen.bbox[2] as f64;
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
            return Ok(Some(alt.block.clone()));
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
        if let Some(dot) = find_dot_if(frame, chosen.x, chosen.y, radius, cfg, valid)? {
            if (dot.0 - chosen.x).abs() > 3.0 || (dot.1 - chosen.y).abs() > 3.0 {
                chosen.x = dot.0;
                chosen.y = dot.1;
                chosen.dot = Some(dot);
            }
        }
    }
    Ok(Some(chosen))
}

fn piece_templates() -> Result<Vec<Frame>, String> {
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
            templates.push(template);
        }
    }
    Ok(templates)
}

fn template_score(frame: &Frame, bbox: [i32; 4], templates: &[Frame]) -> Result<f64, String> {
    if bbox[2].min(bbox[3]) < 6 {
        return Ok(0.0);
    }
    let mut best: f64 = 0.0;
    for tpl in templates {
        let mut score = 0.0;
        let status = unsafe {
            jb_piece_score(
                frame.bgr.as_ptr(),
                frame.width as i32,
                frame.height as i32,
                bbox.as_ptr(),
                tpl.bgr.as_ptr(),
                tpl.width as i32,
                tpl.height as i32,
                &mut score,
            )
        };
        if status != 0 {
            return Err("OpenCV piece template score failed".into());
        }
        best = best.max(score);
    }
    Ok(best)
}

fn template_scan(frame: &Frame, tpl: &Frame, cfg: &Config) -> Result<Option<Piece>, String> {
    let top = (0.30 * frame.height as f64) as usize;
    if tpl.height >= frame.height - top || tpl.width >= frame.width {
        return Ok(None);
    }
    let (mut x, mut y, mut score) = (0, 0, 0.0);
    let status = unsafe {
        jb_piece_scan(
            frame.bgr.as_ptr(),
            frame.width as i32,
            frame.height as i32,
            top as i32,
            tpl.bgr.as_ptr(),
            tpl.width as i32,
            tpl.height as i32,
            &mut x,
            &mut y,
            &mut score,
        )
    };
    if status != 0 {
        return Err("OpenCV piece template scan failed".into());
    }
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
    templates: Vec<Frame>,
    prior: Option<Piece>,
}

impl Vision {
    pub fn new(cfg: &Config, backend: &str) -> Result<Self, String> {
        Ok(Self {
            cfg: cfg.clone(),
            masks: crate::masks::Masks::new(cfg, backend),
            templates: piece_templates()?,
            prior: None,
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
        let piece = self.find_piece(frame, &mut piece_mask, &mut block_mask)?;
        let mut target = None;
        if let Some(p) = &piece {
            if want_target {
                target = find_target(frame, &block_mask, p, &piece_mask, &self.cfg)?;
            }
            self.prior = Some(p.clone());
        }
        Ok(Detection {
            piece,
            target,
            piece_mask,
            block_mask,
        })
    }

    fn find_piece(
        &self,
        frame: &Frame,
        pm: &mut [u8],
        bm: &mut [u8],
    ) -> Result<Option<Piece>, String> {
        let (w, h) = (frame.width as f64, frame.height as f64);
        let cfg = &self.cfg;
        let parts = Regions::new(
            frame.width,
            frame.height,
            "piece components",
            |out| unsafe {
                jb_piece_regions(pm.as_ptr(), frame.width as i32, frame.height as i32, out)
            },
        )?;
        let min_area = cfg.f("piece_min_area_ratio") * (w * h);
        let max_area = cfg.f("piece_max_area_ratio") * (w * h);
        let foot_ratio = cfg.f("piece_min_y_ratio");
        let score_min = cfg.f("piece_template_min_score");
        let auto_skin = cfg.b("piece_skin_shape_enabled");
        let mut candidates = Vec::new();
        let mut horizontal = false;
        for &[x, y, bw, bh, area] in &parts.stats()[1..] {
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
                skin = crate::skin::detect_skin(frame, Some(bm), foot_ratio, !horizontal);
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
            for tpl in &self.templates {
                let Some(hit) = template_scan(frame, tpl, cfg)? else {
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
