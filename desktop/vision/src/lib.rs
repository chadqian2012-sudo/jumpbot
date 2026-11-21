use std::slice;

/// Compute BGR medians for both edge bands of each row.
///
/// # Safety
/// The caller supplies an initialized packed BGR buffer of height * width * 3
/// bytes and two writable, disjoint output buffers of height * 3 floats.
#[no_mangle]
pub unsafe extern "C" fn jumpbot_edge_medians(
    pixels: *const u8,
    height: usize,
    width: usize,
    left_width: usize,
    right_start: usize,
    left: *mut f32,
    right: *mut f32,
) {
    let frame = unsafe { slice::from_raw_parts(pixels, height * width * 3) };
    let left = unsafe { slice::from_raw_parts_mut(left, height * 3) };
    let right = unsafe { slice::from_raw_parts_mut(right, height * 3) };
    let mut channel = Vec::with_capacity(left_width.max(width - right_start));
    for (y, row) in frame.chunks_exact(width * 3).enumerate() {
        for (band, output) in [
            (&row[..left_width * 3], &mut left[y * 3..][..3]),
            (&row[right_start * 3..], &mut right[y * 3..][..3]),
        ] {
            for (c, median) in output.iter_mut().enumerate() {
                channel.clear();
                channel.extend(band[c..].iter().step_by(3).copied());
                let mid = channel.len() / 2;
                let even = channel.len() % 2 == 0;
                let (lower, upper, _) = channel.select_nth_unstable(mid);
                *median = if even {
                    (f32::from(*lower.iter().max().unwrap()) + f32::from(*upper)) * 0.5
                } else {
                    f32::from(*upper)
                };
            }
        }
    }
}

/// Return the lowest fitting edge vertex, or -1 when no edge fits.
///
/// # Safety
/// The mask must contain (limit + 1) * width readable bytes.
#[no_mangle]
pub unsafe extern "C" fn jumpbot_fit_vertex(
    mask: *const u8,
    width: usize,
    top: usize,
    limit: usize,
    corner_x: f64,
    dx_per_dy: f64,
    tolerance: f64,
) -> isize {
    let mask = unsafe { slice::from_raw_parts(mask, (limit + 1) * width) };
    let start = (corner_x - 2.0).ceil() as usize;
    let end = ((corner_x + 2.0).floor() as usize).min(width - 1);
    let mut last_edge = None;
    let mut last_growth = None;
    let mut previous = None;
    let mut misses = 0;
    for y in top + 1..=limit {
        let row = &mask[y * width..][..width];
        // The first occupied pixel inside the tolerance belongs to the first
        // segment accepted by the Python scan. Expand to find its true edges.
        let Some(seed) = (start..=end).find(|&x| row[x] != 0) else {
            break;
        };
        let mut left = seed;
        let mut right = seed;
        while left > 0 && row[left - 1] != 0 {
            left -= 1;
        }
        while right + 1 < width && row[right + 1] != 0 {
            right += 1;
        }
        let span = (y - top) as f64 * dx_per_dy;
        if (left as f64 - (corner_x - span)).abs() <= tolerance
            && (right as f64 - (corner_x + span)).abs() <= tolerance
        {
            last_edge = Some(y);
            misses = 0;
            let distance = if right >= width.saturating_sub(2) {
                corner_x - left as f64
            } else {
                right as f64 - corner_x
            };
            if previous.is_some_and(|value| distance > value) {
                last_growth = Some(y);
            }
            previous = Some(distance);
        } else {
            misses += 1;
            if misses >= 3 {
                break;
            }
        }
    }
    match last_edge {
        Some(y) if y - top >= 6 => last_growth.unwrap_or(y) as isize,
        _ => -1,
    }
}
