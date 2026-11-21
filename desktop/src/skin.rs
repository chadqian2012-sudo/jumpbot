use crate::Frame;
use std::{ffi::c_void, ptr};

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

#[repr(C)]
#[derive(Clone, Copy)]
struct Bounds {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Bounds {
    fn right(self) -> i32 {
        self.x + self.w
    }
    fn bottom(self) -> i32 {
        self.y + self.h
    }
    fn diameter(self) -> f64 {
        f64::from(self.w + self.h) * 0.5
    }
    fn center_x(self) -> f64 {
        f64::from(self.x) + f64::from(self.w) * 0.5
    }
    fn area(self) -> i32 {
        self.w * self.h
    }
}

#[repr(C)]
struct Candidate {
    head_id: usize,
    head: Bounds,
    body: Bounds,
    bounds: Bounds,
    roundness: f64,
    solidity: f64,
    repaired: i32,
    support_count: i32,
    mask: *const u8,
    points: *const [i32; 2],
}

// All callbacks run synchronously on the calling thread. C++ owns the contour
// and pixel buffers; Rust owns the per-frame recognition and ambiguity state.
#[repr(C)]
struct Rules {
    head_kind: extern "C" fn(i32, Bounds, f64, f64) -> i32,
    upright: extern "C" fn(*const c_void, usize, Bounds, Bounds, f64, i32) -> i32,
    material_and_waist: extern "C" fn(*const i32, usize, f64, f64, f64, f64) -> i32,
    consider: extern "C" fn(*mut c_void, *const Candidate),
}

extern "C" {
    fn jb_skin_visit(
        bgr: *const u8,
        width: i32,
        height: i32,
        support: *const u8,
        context: *mut c_void,
        rules: *const Rules,
    ) -> i32;
}

#[derive(Default)]
struct Selection {
    height: i32,
    min_foot_ratio: f64,
    last_original_head: Option<usize>,
    matches: Vec<(Bounds, SkinMatch)>,
}

extern "C" fn head_kind(width: i32, b: Bounds, circularity: f64, solidity: f64) -> i32 {
    let aspect = f64::from(b.w) / f64::from(b.h);
    if !(8f64.max(f64::from(width) * 0.018)..=f64::from(width) * 0.10).contains(&f64::from(b.w))
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

extern "C" fn upright(
    selection: *const c_void,
    head_id: usize,
    h: Bounds,
    b: Bounds,
    solidity: f64,
    repaired: i32,
) -> i32 {
    let selection = unsafe { &*selection.cast::<Selection>() };
    let diameter = h.diameter();
    let gap = f64::from(b.y - h.bottom());
    let width = h.right().max(b.right()) - h.x.min(b.x);
    let aspect = f64::from(b.bottom() - h.y) / f64::from(width);
    i32::from(
        !(repaired != 0 && selection.last_original_head == Some(head_id))
            && (0.55..=1.65).contains(&(f64::from(b.w) / diameter))
            && (1.15..=3.8).contains(&(f64::from(b.h) / diameter))
            && solidity >= 0.86
            && f64::from(b.h) / f64::from(b.w) >= 1.15
            && (-2.0..=diameter * 0.35).contains(&gap)
            && (b.center_x() - h.center_x()).abs() <= diameter * 0.25
            && f64::from(b.bottom()) >= f64::from(selection.height) * selection.min_foot_ratio
            && b.bottom() < selection.height - 2
            && (1.5..=4.8).contains(&aspect),
    )
}

fn median(values: &[i32]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    f64::from(sorted[sorted.len() / 2] + sorted[(sorted.len() - 1) / 2]) * 0.5
}

fn has_waist(rows: &[i32]) -> bool {
    // A body aligned to a qualifying head has at least six rows.
    let h = rows.len();
    let neck = median(&rows[1.max(h / 8)..2.max(h / 3)]);
    let base = median(&rows[h / 2..(h / 2 + 1).max(h * 7 / 8)]);
    if neck >= 3.0 && base >= neck * 1.15 {
        return true;
    }
    let band = 3.max(h / 10);
    let collar = median(&rows[1.max(h / 12)..2.max(h / 6)]);
    let waist = (h / 4..=(h / 2).min(h - band))
        .map(|y| median(&rows[y..y + band]))
        .fold(f64::INFINITY, f64::min);
    let lower = (h / 2..=(h / 2).max(h * 7 / 8 - band))
        .map(|y| median(&rows[y..y + band]))
        .fold(0.0, f64::max);
    (neck >= 3.0 && lower >= neck * 1.15)
        || (waist >= 3.0 && waist < collar && lower >= waist * 1.15)
}

extern "C" fn material_and_waist(
    widths: *const i32,
    height: usize,
    body_distance: f64,
    base_distance: f64,
    base_chroma: f64,
    tolerance: f64,
) -> i32 {
    // A brighter base may still be the same material. A different-colour
    // platform rim under the foot invalidates this proposal.
    i32::from(
        body_distance <= 35.0
            && (base_distance <= 35.0 || base_chroma <= tolerance)
            && has_waist(unsafe { std::slice::from_raw_parts(widths, height) }),
    )
}

fn nested(a: &(Bounds, SkinMatch), b: &(Bounds, SkinMatch)) -> bool {
    let (h, k) = (a.0, b.0);
    let overlap = (h.right().min(k.right()) - h.x.max(k.x)).max(0)
        * (h.bottom().min(k.bottom()) - h.y.max(k.y)).max(0);
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

extern "C" fn consider(selection: *mut c_void, raw: *const Candidate) {
    let selection = unsafe { &mut *selection.cast::<Selection>() };
    let raw = unsafe { &*raw };
    if raw.repaired == 0 {
        selection.last_original_head = Some(raw.head_id);
    }
    let b = raw.bounds;
    let mask = unsafe { std::slice::from_raw_parts(raw.mask, b.area() as usize) }.to_vec();
    let points = unsafe { std::slice::from_raw_parts(raw.points, raw.support_count as usize) };
    let new = (
        raw.head,
        SkinMatch {
            x: raw.body.center_x(),
            y: f64::from(raw.body.bottom()),
            score: raw.roundness.min(raw.solidity),
            bbox: (b.x as usize, b.y as usize, b.w as usize, b.h as usize),
            mask,
            support_points: points
                .iter()
                .map(|&[x, y]| (x as usize, y as usize))
                .collect(),
        },
    );
    for old in &mut selection.matches {
        let enclosed = nested(old, &new);
        let close = (old.1.x - new.1.x).hypot(old.1.y - new.1.y) < raw.head.diameter() * 0.35;
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

/// An absent support mask requests shape-only recognition. With a mask,
/// even a geometrically valid person must have evidence of a platform.
pub fn detect_skin(
    frame: &Frame,
    support: Option<&[u8]>,
    min_foot_ratio: f64,
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
    const RULES: Rules = Rules {
        head_kind,
        upright,
        material_and_waist,
        consider,
    };
    let mut selection = Selection {
        height,
        min_foot_ratio,
        ..Selection::default()
    };
    let status = unsafe {
        jb_skin_visit(
            frame.bgr.as_ptr(),
            width,
            height,
            support.map_or(ptr::null(), |m| m.as_ptr()),
            (&mut selection as *mut Selection).cast(),
            &RULES,
        )
    };
    if status != 1 || selection.matches.len() != 1 {
        return None;
    }
    selection.matches.pop().map(|(_, m)| m)
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
            let m = detect_skin(&fixture(name), None, 0.5).expect(name);
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
        let m = detect_skin(&frame, None, 0.5).unwrap();
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
            detect_skin(&frame, None, 0.5).is_none(),
            "two complete people"
        );
        let mut frame = fixture("skin-gold-yellow-badge");
        let background = frame.bgr[(100 * frame.width + 460) * 3..][..3].to_vec();
        for row in 575..611 {
            for x in 345..385 {
                frame.bgr[(row * frame.width + x) * 3..][..3].copy_from_slice(&background);
            }
        }
        assert!(detect_skin(&frame, None, 0.5).is_none(), "missing body");
    }

    #[test]
    fn support_requires_a_valid_mask_and_two_rows_under_one_foot() {
        let frame = fixture("skin-gold-narrow-pylon");
        let mut support = vec![0; frame.width * frame.height];
        assert!(detect_skin(&frame, Some(&support), 0.5).is_none());
        support.fill(255);
        assert!(detect_skin(&frame, Some(&support), 0.5).is_none());
        support.fill(0);
        support[(649 + 6) * frame.width + 166] = 255;
        assert!(detect_skin(&frame, Some(&support), 0.5).is_none());
        support[(649 + 10) * frame.width + 166] = 255;
        let m = detect_skin(&frame, Some(&support), 0.5).unwrap();
        assert_eq!((m.x, m.y), (166., 649.));
        assert!(m.support_points.is_empty());

        // A broad outlined platform can fill in precisely the missing rows.
        let frame = fixture("skin-gold-play");
        let mut outside = vec![0; frame.width * frame.height];
        outside[0] = 255;
        let m = detect_skin(&frame, Some(&outside), 0.5).unwrap();
        let xs = m.x.round_ties_even() as usize;
        let ys = m.y as usize;
        let expected: Vec<_> = [6, 10, 14, 18, 24]
            .into_iter()
            .flat_map(|dy| (xs - 4..=xs + 4).map(move |x| (x, ys + dy)))
            .collect();
        assert_eq!(m.support_points, expected);
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
}
