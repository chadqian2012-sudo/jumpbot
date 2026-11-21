//! Binary masks stored in horizontal words. The morphology kernels visit 64 pixels at once.
#[derive(Clone)]
pub(crate) struct Mask {
    width: usize,
    height: usize,
    stride: usize,
    bits: Vec<u64>,
}

impl Mask {
    pub(crate) fn from_pixels(pixels: &[u8], width: usize, height: usize) -> Self {
        let stride = width.div_ceil(64);
        let mut bits = Vec::with_capacity(stride * height);
        for row in pixels.chunks_exact(width) {
            for word in row.chunks(64) {
                let mut value = 0u64;
                let mut start = 0;
                #[cfg(target_arch = "x86_64")]
                {
                    use std::arch::x86_64::*;
                    for lane in word.chunks_exact(16) {
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
    fn grow(&mut self, radius: usize) {
        let mut scratch = vec![0; self.bits.len()];
        let mut covered = 0;
        while covered < radius {
            let shift = (2 * covered + 1).min(radius - covered);
            for (row, out) in self
                .bits
                .chunks_exact(self.stride)
                .zip(scratch.chunks_exact_mut(self.stride))
            {
                for (i, dst) in out.iter_mut().enumerate() {
                    let here = row[i];
                    let left = if i > 0 { row[i - 1] >> (64 - shift) } else { 0 };
                    let right = row.get(i + 1).copied().unwrap_or(0) << (64 - shift);
                    *dst = here | (here << shift) | (here >> shift) | left | right;
                }
            }
            std::mem::swap(&mut self.bits, &mut scratch);
            self.trim();
            let step = shift * self.stride;
            scratch.copy_from_slice(&self.bits);
            if shift < self.height {
                let n = self.bits.len() - step;
                for (out, src) in scratch[step..].iter_mut().zip(&self.bits[..n]) {
                    *out |= *src;
                }
                for (out, src) in scratch[..n].iter_mut().zip(&self.bits[step..]) {
                    *out |= *src;
                }
            }
            std::mem::swap(&mut self.bits, &mut scratch);
            covered += shift;
        }
    }

    fn erode(&mut self, radius: usize) {
        self.invert();
        self.grow(radius);
        self.invert();
    }

    pub(crate) fn piece(mut self) -> Regions {
        self.erode(2);
        self.grow(4);
        self.erode(2);
        self.regions()
    }

    pub(crate) fn blocks(mut self, mut piece: Mask) -> Regions {
        piece.grow(8);
        for (dst, pawn) in self.bits.iter_mut().zip(piece.bits) {
            *dst &= !pawn;
        }
        self.grow(4);
        self.erode(5);
        self.grow(1);
        // An exterior contour includes enclosed holes. The eight-connected
        // foreground has a four-connected background, including at diagonals.
        self.invert();
        let (spans, mut parents) = self.spans(false);
        let mut exterior = vec![false; parents.len()];
        for span in &spans {
            if span.y == 0 || span.y + 1 == self.height || span.x == 0 || span.end == self.width {
                exterior[root(&mut parents, span.id)] = true;
            }
        }
        self.bits.fill(u64::MAX);
        self.trim();
        for span in spans {
            if exterior[root(&mut parents, span.id)] {
                let row = &mut self.bits[span.y * self.stride..][..self.stride];
                let last = (span.end - 1) / 64;
                for (i, word) in row.iter_mut().enumerate().take(last + 1).skip(span.x / 64) {
                    let a = span.x.saturating_sub(i * 64);
                    let b = (span.end - i * 64).min(64);
                    *word &= !((u64::MAX << a) & (u64::MAX >> (64 - b)));
                }
            }
        }
        self.regions()
    }

    fn spans(&self, diagonal: bool) -> (Vec<Span>, Vec<usize>) {
        let mut spans: Vec<Span> = Vec::new();
        let mut parents = Vec::new();
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
                    let a = root(&mut parents, id);
                    let b = root(&mut parents, old.id);
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
        (spans, parents)
    }

    pub(crate) fn regions(&self) -> Regions {
        let (spans, mut parents) = self.spans(true);
        let mut ids = vec![0; parents.len()];
        let mut stats = vec![[0; 5]];
        let mut labels = vec![0; self.width * self.height];
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
            labels[span.y * self.width + span.x..span.y * self.width + span.end].fill(id as i32);
        }
        for s in &mut stats[1..] {
            s[2] -= s[0];
            s[3] -= s[1];
        }
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
    }
}
