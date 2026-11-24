//! Binary masks stored in horizontal words. The morphology kernels visit 64 pixels at once.
#[derive(Clone)]
pub(crate) struct Mask {
    width: usize,
    height: usize,
    stride: usize,
    bits: Vec<u64>,
}

#[derive(Default)]
pub(crate) struct RegionScratch {
    morph: Vec<u64>,
    spans: Vec<Span>,
    parents: Vec<usize>,
    exterior: Vec<bool>,
    ids: Vec<usize>,
    labels: Vec<i32>,
}

pub(crate) struct PieceRegions {
    mask: Mask,
    pub(crate) stats: Vec<[i32; 5]>,
}

impl PieceRegions {
    pub(crate) fn foreground_in(&self, bounds: [i32; 4], silhouette: Option<&[u8]>) -> usize {
        let [x, y, w, h] = bounds.map(|n| n as usize);
        let mut count = 0;
        if let Some(mask) = silhouette {
            for row in 0..h {
                let words = &self.mask.bits[(y + row) * self.mask.stride..][..self.mask.stride];
                let sil = &mask[row * w..][..w];
                for (dx, &occupied) in sil.iter().enumerate() {
                    if occupied != 0 {
                        let px = x + dx;
                        count += usize::from(((words[px / 64] >> (px % 64)) & 1) != 0);
                    }
                }
            }
        } else if w > 0 {
            let end = x + w;
            let first_word = x / 64;
            let last_word = (end - 1) / 64;
            let lo_mask = u64::MAX << (x % 64);
            let hi_mask = u64::MAX >> ((64 - (end % 64)) % 64);
            for row in 0..h {
                let words = &self.mask.bits[(y + row) * self.mask.stride..][..self.mask.stride];
                if first_word == last_word {
                    count += (words[first_word] & lo_mask & hi_mask).count_ones() as usize;
                } else {
                    count += (words[first_word] & lo_mask).count_ones() as usize;
                    for &word in &words[first_word + 1..last_word] {
                        count += word.count_ones() as usize;
                    }
                    count += (words[last_word] & hi_mask).count_ones() as usize;
                }
            }
        }
        count
    }
}

impl Mask {
    pub(crate) fn from_pixels(pixels: &[u8], width: usize, height: usize) -> Self {
        let stride = width.div_ceil(64);
        let mut bits = Vec::with_capacity(stride * height);
        #[cfg(target_arch = "x86_64")]
        let has_avx2 = is_x86_feature_detected!("avx2");
        for row in pixels.chunks_exact(width) {
            for word in row.chunks(64) {
                let mut value = 0u64;
                let mut start = 0;
                #[cfg(target_arch = "x86_64")]
                {
                    use std::arch::x86_64::*;
                    if has_avx2 {
                        for lane in word.as_chunks::<32>().0 {
                            // AVX2 was detected above and each 32-byte load stays within `lane`.
                            let nonzero = unsafe {
                                let v = _mm256_loadu_si256(lane.as_ptr().cast());
                                !(_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_setzero_si256()))
                                    as u32)
                            };
                            value |= u64::from(nonzero) << start;
                            start += 32;
                        }
                    }
                    for lane in word[start..].as_chunks::<16>().0 {
                        // SSE2 is part of x86-64. Each load is wholly inside this slice.
                        let nonzero = unsafe {
                            let v = _mm_loadu_si128(lane.as_ptr().cast());
                            !_mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_setzero_si128())) as u16
                        };
                        value |= u64::from(nonzero) << start;
                        start += 16;
                    }
                }
                for (i, &p) in word[start..].iter().enumerate() {
                    value |= u64::from(p != 0) << (start + i);
                }
                bits.push(value);
            }
        }
        Self {
            width,
            height,
            stride,
            bits,
        }
    }

    fn trim(&mut self) {
        let tail = self.width % 64;
        if tail != 0 {
            let mask = (1u64 << tail) - 1;
            for row in self.bits.chunks_exact_mut(self.stride) {
                row[self.stride - 1] &= mask;
            }
        }
    }

    fn invert(&mut self) {
        self.bits.iter_mut().for_each(|word| *word = !*word);
        self.trim();
    }

    /// Grow a square by Chebyshev distance, doubling the covered interval at each pass.
    fn grow_with(&mut self, radius: usize, scratch: &mut Vec<u64>) {
        scratch.resize(self.bits.len(), 0);
        let mut covered = 0;
        while covered < radius {
            let shift = (2 * covered + 1).min(radius - covered);
            let inv_shift = 64 - shift;
            for (row, out) in self
                .bits
                .chunks_exact(self.stride)
                .zip(scratch.chunks_exact_mut(self.stride))
            {
                if self.stride == 1 {
                    let here = row[0];
                    out[0] = here | (here << shift) | (here >> shift);
                } else if self.stride >= 2 {
                    out[0] = row[0] | (row[0] << shift) | (row[0] >> shift) | (row[1] << inv_shift);
                    let last = self.stride - 1;
                    for i in 1..last {
                        let here = row[i];
                        out[i] = here
                            | (here << shift)
                            | (here >> shift)
                            | (row[i - 1] >> inv_shift)
                            | (row[i + 1] << inv_shift);
                    }
                    let here = row[last];
                    out[last] =
                        here | (here << shift) | (here >> shift) | (row[last - 1] >> inv_shift);
                }
            }
            std::mem::swap(&mut self.bits, scratch);
            self.trim();
            if shift < self.height {
                let step = shift * self.stride;
                let n = self.bits.len() - step;
                let bits = &self.bits;
                if step < n {
                    for (out, (&a, &b)) in scratch[..step]
                        .iter_mut()
                        .zip(bits[..step].iter().zip(&bits[step..2 * step]))
                    {
                        *out = a | b;
                    }
                    for (out, ((&a, &lo), &hi)) in scratch[step..n].iter_mut().zip(
                        bits[step..n]
                            .iter()
                            .zip(&bits[..n - step])
                            .zip(&bits[2 * step..]),
                    ) {
                        *out = a | lo | hi;
                    }
                    for (out, (&a, &b)) in scratch[n..]
                        .iter_mut()
                        .zip(bits[n..].iter().zip(&bits[n - step..n]))
                    {
                        *out = a | b;
                    }
                } else {
                    for (out, (&a, &b)) in scratch[..n]
                        .iter_mut()
                        .zip(bits[..n].iter().zip(&bits[step..]))
                    {
                        *out = a | b;
                    }
                    scratch[n..step].copy_from_slice(&bits[n..step]);
                    for (out, (&a, &b)) in scratch[step..]
                        .iter_mut()
                        .zip(bits[step..].iter().zip(&bits[..n]))
                    {
                        *out = a | b;
                    }
                }
                std::mem::swap(&mut self.bits, scratch);
            }
            covered += shift;
        }
    }

    #[cfg(test)]
    fn grow(&mut self, radius: usize) {
        let mut scratch = Vec::new();
        self.grow_with(radius, &mut scratch);
    }

    fn erode_with(&mut self, radius: usize, scratch: &mut Vec<u64>) {
        self.invert();
        self.grow_with(radius, scratch);
        self.invert();
    }

    #[cfg(test)]
    fn erode(&mut self, radius: usize) {
        let mut scratch = Vec::new();
        self.erode_with(radius, &mut scratch);
    }

    pub(crate) fn piece(mut self) -> PieceRegions {
        let mut scratch = Vec::new();
        self.erode_with(2, &mut scratch);
        self.grow_with(4, &mut scratch);
        self.erode_with(2, &mut scratch);
        let stats = self.stats_only(true);
        PieceRegions { mask: self, stats }
    }

    fn prepare_blocks(&mut self, mut piece: Mask, scratch: &mut RegionScratch) {
        piece.grow_with(8, &mut scratch.morph);
        for (dst, pawn) in self.bits.iter_mut().zip(piece.bits) {
            *dst &= !pawn;
        }
        self.grow_with(4, &mut scratch.morph);
        self.erode_with(5, &mut scratch.morph);
        self.grow_with(1, &mut scratch.morph);
        // An exterior contour includes enclosed holes. The eight-connected
        // foreground has a four-connected background, including at diagonals.
        self.invert();
        self.spans_into(false, &mut scratch.spans, &mut scratch.parents);
        scratch.exterior.clear();
        scratch.exterior.resize(scratch.parents.len(), false);
        for span in &scratch.spans {
            if span.y == 0 || span.y + 1 == self.height || span.x == 0 || span.end == self.width {
                scratch.exterior[root(&mut scratch.parents, span.id)] = true;
            }
        }
        self.bits.fill(u64::MAX);
        self.trim();
        for span in &scratch.spans {
            if scratch.exterior[root(&mut scratch.parents, span.id)] {
                let row = &mut self.bits[span.y * self.stride..][..self.stride];
                let last = (span.end - 1) / 64;
                for (i, word) in row.iter_mut().enumerate().take(last + 1).skip(span.x / 64) {
                    let a = span.x.saturating_sub(i * 64);
                    let b = (span.end - i * 64).min(64);
                    *word &= !((u64::MAX << a) & (u64::MAX >> (64 - b)));
                }
            }
        }
    }

    pub(crate) fn blocks(mut self, piece: Mask) -> Regions {
        let mut scratch = RegionScratch::default();
        self.prepare_blocks(piece, &mut scratch);
        self.regions()
    }

    pub(crate) fn blocks_with<'a>(
        mut self,
        piece: Mask,
        scratch: &'a mut RegionScratch,
    ) -> RegionsRef<'a> {
        self.prepare_blocks(piece, scratch);
        self.regions_with(scratch)
    }

    fn spans_into(&self, diagonal: bool, spans: &mut Vec<Span>, parents: &mut Vec<usize>) {
        spans.clear();
        parents.clear();
        let min_cap = self.height.saturating_mul(2);
        if spans.capacity() < min_cap {
            spans.reserve(min_cap - spans.len());
        }
        if parents.capacity() < min_cap {
            parents.reserve(min_cap - parents.len());
        }
        let mut previous = 0..0;
        for (y, row) in self.bits.chunks_exact(self.stride).enumerate() {
            let begin = spans.len();
            let mut next = previous.start;
            let mut x = 0;
            while x < self.width {
                let remaining = row[x / 64] >> (x % 64);
                if remaining == 0 {
                    x = (x / 64 + 1) * 64;
                    continue;
                }
                x += remaining.trailing_zeros() as usize;
                let start = x;
                loop {
                    let word = row[x / 64] >> (x % 64);
                    let n = word.trailing_ones() as usize;
                    x += n;
                    if x >= self.width || x % 64 != 0 || n == 0 {
                        break;
                    }
                }
                let id = parents.len();
                parents.push(id);
                let margin = usize::from(diagonal);
                while next < previous.end && spans[next].end + margin <= start {
                    next += 1;
                }
                for old in &spans[next..previous.end] {
                    if old.x >= x + margin {
                        break;
                    }
                    let a = root(parents, id);
                    let b = root(parents, old.id);
                    parents[a.max(b)] = a.min(b);
                }
                spans.push(Span {
                    x: start,
                    end: x,
                    y,
                    id,
                });
            }
            previous = begin..spans.len();
        }
    }

    fn stats_only(&self, diagonal: bool) -> Vec<[i32; 5]> {
        let (mut spans, mut parents) = (Vec::new(), Vec::new());
        self.spans_into(diagonal, &mut spans, &mut parents);
        let mut ids = vec![0; parents.len()];
        let mut stats = vec![[0; 5]];
        for span in spans {
            let parent = root(&mut parents, span.id);
            let id = if ids[parent] == 0 {
                let id = stats.len();
                ids[parent] = id;
                stats.push([
                    span.x as i32,
                    span.y as i32,
                    span.end as i32,
                    span.y as i32 + 1,
                    0,
                ]);
                id
            } else {
                ids[parent]
            };
            let s = &mut stats[id];
            s[0] = s[0].min(span.x as i32);
            s[2] = s[2].max(span.end as i32);
            s[3] = span.y as i32 + 1;
            s[4] += (span.end - span.x) as i32;
        }
        for s in &mut stats[1..] {
            s[2] -= s[0];
            s[3] -= s[1];
        }
        stats
    }

    fn regions_with<'a>(&self, scratch: &'a mut RegionScratch) -> RegionsRef<'a> {
        self.spans_into(true, &mut scratch.spans, &mut scratch.parents);
        scratch.ids.clear();
        scratch.ids.resize(scratch.parents.len(), 0);
        let mut stats = vec![[0; 5]];
        let needed = self.width * self.height;
        scratch.labels.resize(needed, 0);
        scratch.labels[..needed].fill(0);
        for span in &scratch.spans {
            let parent = root(&mut scratch.parents, span.id);
            let id = if scratch.ids[parent] == 0 {
                let id = stats.len();
                scratch.ids[parent] = id;
                stats.push([
                    span.x as i32,
                    span.y as i32,
                    span.end as i32,
                    span.y as i32 + 1,
                    0,
                ]);
                id
            } else {
                scratch.ids[parent]
            };
            let s = &mut stats[id];
            s[0] = s[0].min(span.x as i32);
            s[2] = s[2].max(span.end as i32);
            s[3] = span.y as i32 + 1;
            s[4] += (span.end - span.x) as i32;
            scratch.labels[span.y * self.width + span.x..span.y * self.width + span.end]
                .fill(id as i32);
        }
        for s in &mut stats[1..] {
            s[2] -= s[0];
            s[3] -= s[1];
        }
        RegionsRef {
            width: self.width,
            labels: &scratch.labels[..needed],
            stats,
        }
    }

    pub(crate) fn regions(&self) -> Regions {
        let mut scratch = RegionScratch::default();
        let res = self.regions_with(&mut scratch);
        let stats = res.stats;
        let labels = std::mem::take(&mut scratch.labels);
        Regions {
            width: self.width,
            stats,
            labels,
        }
    }
}

struct Span {
    x: usize,
    end: usize,
    y: usize,
    id: usize,
}

fn root(parents: &mut [usize], mut i: usize) -> usize {
    while parents[i] != i {
        parents[i] = parents[parents[i]];
        i = parents[i];
    }
    i
}

pub(crate) struct Regions {
    pub(crate) width: usize,
    pub(crate) labels: Vec<i32>,
    pub(crate) stats: Vec<[i32; 5]>,
}

pub(crate) struct RegionsRef<'a> {
    pub(crate) width: usize,
    pub(crate) labels: &'a [i32],
    pub(crate) stats: Vec<[i32; 5]>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boundaries_and_diagonal_holes_match_pixel_neighborhoods() {
        use std::collections::VecDeque;
        fn step(p: &[u8], w: usize, r: usize, grow: bool) -> Vec<u8> {
            let h = p.len() / w;
            (0..p.len())
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let any = (y.saturating_sub(r)..=(y + r).min(h - 1)).any(|yy| {
                        p[yy * w + x.saturating_sub(r)..=yy * w + (x + r).min(w - 1)]
                            .iter()
                            .any(|v| (*v != 0) == grow)
                    });
                    u8::from(any == grow)
                })
                .collect()
        }
        for w in [1, 3, 63, 64, 65, 127, 129] {
            let h = 17;
            let mut original = vec![0; w * h];
            for (i, p) in original.iter_mut().enumerate() {
                let (x, y) = (i % w, i / w);
                *p = u8::from(
                    (3..w.saturating_sub(3)).contains(&x)
                        && (2..15).contains(&y)
                        && ((x / 11 + y / 7) % 4 != 2),
                );
            }
            let make = || Mask::from_pixels(&original, w, h);
            for radius in [1, 2, 4, 5, 8] {
                for grow in [false, true] {
                    let mut mask = make();
                    if grow {
                        mask.grow(radius)
                    } else {
                        mask.erode(radius)
                    }
                    let actual = mask
                        .regions()
                        .labels
                        .into_iter()
                        .map(|v| u8::from(v != 0))
                        .collect::<Vec<_>>();
                    assert_eq!(
                        actual,
                        step(&original, w, radius, grow),
                        "width {w} radius {radius}, dilation {grow}"
                    );
                }
            }
            let expected = step(
                &step(
                    &step(&step(&original, w, 4, true), w, 4, false),
                    w,
                    1,
                    false,
                ),
                w,
                1,
                true,
            );
            let mut outside = vec![false; w * h];
            let mut queue = VecDeque::new();
            for (i, &v) in expected.iter().enumerate() {
                if v == 0 && (i % w == 0 || i % w == w - 1 || i / w == 0 || i / w == h - 1) {
                    outside[i] = true;
                    queue.push_back(i);
                }
            }
            while let Some(i) = queue.pop_front() {
                for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let (x, y) = ((i % w) as isize + dx, (i / w) as isize + dy);
                    if (0..w as isize).contains(&x) && (0..h as isize).contains(&y) {
                        let j = y as usize * w + x as usize;
                        if !outside[j] && expected[j] == 0 {
                            outside[j] = true;
                            queue.push_back(j);
                        }
                    }
                }
            }
            let result = make().blocks(Mask::from_pixels(&vec![0; w * h], w, h));
            assert!(
                outside
                    .iter()
                    .zip(result.labels)
                    .all(|(&ex, id)| ex == (id == 0)),
                "exterior w={w}"
            );
        }

        fn reference_unfused_grow(p: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
            let mut cur = p.to_vec();
            let mut scratch = vec![0u8; w * h];
            let mut covered = 0;
            while covered < radius {
                let shift = (2 * covered + 1).min(radius - covered);
                for y in 0..h {
                    for x in 0..w {
                        let lo = x.saturating_sub(shift);
                        let hi = (x + shift).min(w - 1);
                        let any_h = (cur[y * w + x] != 0)
                            || (lo < x && cur[y * w + lo] != 0)
                            || (hi > x && cur[y * w + hi] != 0)
                            || cur[y * w + lo..=y * w + hi].iter().any(|&v| v != 0);
                        scratch[y * w + x] = u8::from(any_h);
                    }
                }
                cur.copy_from_slice(&scratch);
                if shift < h {
                    let step = shift * w;
                    let n = cur.len() - step;
                    for (out, src) in scratch[step..].iter_mut().zip(&cur[..n]) {
                        *out |= *src;
                    }
                    for (out, src) in scratch[..n].iter_mut().zip(&cur[step..]) {
                        *out |= *src;
                    }
                    cur.copy_from_slice(&scratch);
                }
                covered += shift;
            }
            cur
        }

        for h in [1, 2, 3, 4, 5, 6] {
            for w in [1, 64, 65] {
                let mut original = vec![0; w * h];
                for (i, p) in original.iter_mut().enumerate() {
                    *p = u8::from((i * 7 + i / w) % 5 == 0);
                }
                for radius in [1, 2, 4, 5, 8] {
                    for grow in [false, true] {
                        let mut mask = Mask::from_pixels(&original, w, h);
                        let expected = if grow {
                            mask.grow(radius);
                            reference_unfused_grow(&original, w, h, radius)
                        } else {
                            mask.erode(radius);
                            let inv: Vec<u8> = original.iter().map(|&v| u8::from(v == 0)).collect();
                            reference_unfused_grow(&inv, w, h, radius)
                                .into_iter()
                                .map(|v| u8::from(v == 0))
                                .collect()
                        };
                        let actual: Vec<_> = mask
                            .regions()
                            .labels
                            .into_iter()
                            .map(|v| u8::from(v != 0))
                            .collect();
                        assert_eq!(
                            actual, expected,
                            "short {w}x{h} radius {radius}, dilation {grow}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn from_pixels_avx2_handles_sign_bit_lanes_15_31_32_63() {
        for w in [32, 48, 63, 64, 65, 130] {
            let h = 5;
            let mut pixels = vec![0u8; w * h];
            for y in 0..h {
                for &lane in &[0usize, 15, 31, 32, 47, 63, 64, 95, 127] {
                    if lane < w {
                        pixels[y * w + lane] = 255;
                    }
                }
            }
            let mask = Mask::from_pixels(&pixels, w, h);
            let labels = mask.regions().labels;
            let got: Vec<u8> = labels.into_iter().map(|l| u8::from(l != 0)).collect();
            let want: Vec<u8> = pixels.iter().map(|&p| u8::from(p != 0)).collect();
            assert_eq!(got, want, "width={w}");
        }
    }

    #[test]
    fn piece_regions_foreground_in_matches_labeled_regions() {
        for w in [63, 64, 65, 129] {
            let h = 40;
            let mut pixels = vec![0u8; w * h];
            for y in 4..36 {
                for x in 3..w - 3 {
                    pixels[y * w + x] = u8::from((x / 13 + y / 9) % 3 != 0);
                }
            }
            let piece_regions = Mask::from_pixels(&pixels, w, h).piece();
            let mut m = Mask::from_pixels(&pixels, w, h);
            let mut s = Vec::new();
            m.erode_with(2, &mut s);
            m.grow_with(4, &mut s);
            m.erode_with(2, &mut s);
            let labeled = m.regions();
            assert!(labeled.stats.len() > 1, "fixture lost all foreground at w={w}");
            assert_eq!(piece_regions.stats, labeled.stats, "stats w={w}");
            for &(bx, by, bw, bh) in &[
                (0i32, 0i32, w as i32, h as i32),
                (5, 6, 30.min(w as i32 - 5), 18),
                (31, 10, (w as i32 - 33).max(1), 15),
                (10, 12, 1, 10),
            ] {
                let expected_none = (0..bh as usize)
                    .map(|dy| {
                        let start = (by as usize + dy) * w + bx as usize;
                        labeled.labels[start..start + bw as usize]
                            .iter()
                            .filter(|&&l| l != 0)
                            .count()
                    })
                    .sum::<usize>();
                assert_eq!(
                    piece_regions.foreground_in([bx, by, bw, bh], None),
                    expected_none,
                    "None w={w} bbox=({bx},{by},{bw},{bh})"
                );
                let sil: Vec<u8> = (0..(bw * bh) as usize)
                    .map(|i| u8::from(i % 3 != 0))
                    .collect();
                let expected_sil = (0..bh as usize)
                    .map(|dy| {
                        let start = (by as usize + dy) * w + bx as usize;
                        labeled.labels[start..start + bw as usize]
                            .iter()
                            .zip(&sil[dy * bw as usize..][..bw as usize])
                            .filter(|(&l, &o)| l != 0 && o != 0)
                            .count()
                    })
                    .sum::<usize>();
                assert_eq!(
                    piece_regions.foreground_in([bx, by, bw, bh], Some(&sil)),
                    expected_sil,
                    "Some w={w} bbox=({bx},{by},{bw},{bh})"
                );
            }
        }
    }
}
