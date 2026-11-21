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
