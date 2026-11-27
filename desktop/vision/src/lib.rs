fn slice_median(values: &mut [u8]) -> f32 {
    let mid = values.len() / 2;
    let even = values.len().is_multiple_of(2);
    let (lower, upper, _) = values.select_nth_unstable(mid);
    if even {
        (f32::from(*lower.iter().max().unwrap()) + f32::from(*upper)) * 0.5
    } else {
        f32::from(*upper)
    }
}

/// Compute the BGR median on each side of every row.
pub fn edge_medians(
    frame: &[u8],
    width: usize,
    left_width: usize,
    right_start: usize,
    left: &mut [f32],
    right: &mut [f32],
) {
    let mut fallback: [Vec<u8>; 3] = Default::default();
    for (y, row) in frame.chunks_exact(width * 3).enumerate() {
        for (band, output) in [
            (&row[..left_width * 3], &mut left[y * 3..][..3]),
            (&row[right_start * 3..], &mut right[y * 3..][..3]),
        ] {
            let pixels = band.as_chunks::<3>().0;
            let n = pixels.len();
            // Equal slices shifted by one pixel mean the entire band has one BGR color.
            if n != 0 && pixels[1..] == pixels[..n - 1] {
                output.copy_from_slice(&pixels[0].map(f32::from));
                continue;
            }
            if n <= 32 {
                let mut buf = [[0u8; 32]; 3];
                for (i, p) in pixels.iter().enumerate() {
                    buf[0][i] = p[0];
                    buf[1][i] = p[1];
                    buf[2][i] = p[2];
                }
                for (c, median) in output.iter_mut().enumerate() {
                    *median = slice_median(&mut buf[c][..n]);
                }
            } else {
                for buf in &mut fallback {
                    buf.clear();
                    buf.reserve(n);
                }
                for p in pixels {
                    fallback[0].push(p[0]);
                    fallback[1].push(p[1]);
                    fallback[2].push(p[2]);
                }
                for (c, median) in output.iter_mut().enumerate() {
                    *median = slice_median(&mut fallback[c]);
                }
            }
        }
    }
}

/// Return the lowest fitting vertex, or -1 when neither edge fits.
pub fn fit_vertex(
    mask: &[u8],
    width: usize,
    top: usize,
    limit: usize,
    corner_x: f64,
    dx_per_dy: f64,
    tolerance: f64,
) -> isize {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_edge_medians(
        frame: &[u8],
        width: usize,
        left_width: usize,
        right_start: usize,
    ) -> (Vec<f32>, Vec<f32>) {
        let height = frame.len() / (width * 3);
        let mut left = vec![0.0f32; height * 3];
        let mut right = vec![0.0f32; height * 3];
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
        (left, right)
    }

    #[test]
    fn edge_medians_matches_scalar_reference_for_small_and_large_bands() {
        for (width, band) in [(1, 1), (17, 3), (40, 4), (553, 17), (1066, 32), (1200, 36)] {
            let height = 9;
            let mut frame: Vec<u8> = (0..width * height * 3)
                .map(|i| ((i * 73 + i / 11 + 19) % 256) as u8)
                .collect();
            for (y, row) in frame.chunks_exact_mut(width * 3).enumerate().take(4) {
                for p in row.as_chunks_mut::<3>().0 {
                    p.copy_from_slice(&[11, y as u8, 231]);
                }
                if y % 2 == 1 {
                    row[band * 3 - 1] ^= 0xff;
                }
            }
            let (exp_left, exp_right) = reference_edge_medians(&frame, width, band, width - band);
            let mut left = vec![0.0f32; height * 3];
            let mut right = vec![0.0f32; height * 3];
            edge_medians(&frame, width, band, width - band, &mut left, &mut right);
            assert_eq!(
                left.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                exp_left.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "left mismatch w={width} band={band}"
            );
            assert_eq!(
                right.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                exp_right.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "right mismatch w={width} band={band}"
            );
        }
    }
}
