use super::{scalar_row, Tables};
use std::arch::x86_64::*;

#[target_feature(enable = "avx2")]
unsafe fn store_mask(mask: __m256i, out: *mut u8) {
    let words = _mm_packs_epi32(
        _mm256_castsi256_si128(mask),
        _mm256_extracti128_si256::<1>(mask),
    );
    let bytes = _mm_packs_epi16(words, words);
    _mm_storel_epi64(out.cast(), bytes);
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn row(
    row: &[u8],
    bg: &[u8],
    base: [i32; 3],
    table: &Tables,
    piece: &mut [u8],
    block: &mut [u8],
) {
    let splat = |n| _mm256_set1_epi32(n);
    let add = |a, b| _mm256_add_epi32(a, b);
    let sub = |a, b| _mm256_sub_epi32(a, b);
    let gt = |a, b| _mm256_cmpgt_epi32(a, b);
    let and = |a, b| _mm256_and_si256(a, b);
    let or = |a, b| _mm256_or_si256(a, b);
    let invert = |a| _mm256_xor_si256(a, splat(-1));
    let abs = |a| _mm256_abs_epi32(a);
    let [bh, bs, bv] = base.map(splat);
    let threshold = &table[512..];
    let t = |i: usize| splat(threshold[i]);
    let zero = splat(0);
    let byte = splat(255);
    // Each pair of overlapping 16-byte loads stays inside the 24-byte group.
    // Shuffle four packed BGR pixels into four little-endian 32-bit lanes per half.
    let lo = _mm_setr_epi8(0, 1, 2, -1, 3, 4, 5, -1, 6, 7, 8, -1, 9, 10, 11, -1);
    let hi = _mm_setr_epi8(4, 5, 6, -1, 7, 8, 9, -1, 10, 11, 12, -1, 13, 14, 15, -1);
    let mut x = 0;
    while x + 8 <= piece.len() {
        let p = row.as_ptr().add(3 * x);
        let a = _mm_shuffle_epi8(_mm_loadu_si128(p.cast()), lo);
        let b = _mm_shuffle_epi8(_mm_loadu_si128(p.add(8).cast()), hi);
        let packed = _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(a), b);
        let blue = and(packed, byte);
        let green = and(_mm256_srli_epi32::<8>(packed), byte);
        let red = _mm256_srli_epi32::<16>(packed);
        let v = _mm256_max_epi32(blue, _mm256_max_epi32(green, red));
        let delta = sub(v, _mm256_min_epi32(blue, _mm256_min_epi32(green, red)));
        let mut raw = add(sub(red, green), _mm256_slli_epi32::<2>(delta));
        raw = _mm256_blendv_epi8(
            raw,
            add(sub(blue, red), add(delta, delta)),
            _mm256_cmpeq_epi32(v, green),
        );
        raw = _mm256_blendv_epi8(raw, sub(green, blue), _mm256_cmpeq_epi32(v, red));
        let scale = |a, idx, offset| {
            let div = _mm256_i32gather_epi32::<4>(table.as_ptr().add(offset), idx);
            _mm256_srai_epi32::<12>(add(_mm256_mullo_epi32(a, div), splat(2048)))
        };
        let s = scale(delta, v, 0);
        let h = scale(raw, delta, 256);
        let h = add(h, and(gt(zero, h), splat(180)));
        let piece_bits = and(
            or(
                and(gt(s, t(9)), gt(t(10), v)),
                and(gt(s, splat(25)), gt(splat(110), v)),
            ),
            invert(or(gt(t(11), h), gt(h, t(12)))),
        );
        let diff = add(
            abs(sub(blue, splat(bg[0].into()))),
            add(
                abs(sub(green, splat(bg[1].into()))),
                abs(sub(red, splat(bg[2].into()))),
            ),
        );
        let dark = gt(sub(bv, v), t(1));
        let hue = and(gt(abs(sub(h, bh)), t(2)), gt(s, t(3)));
        let chroma = or(gt(sub(s, bs), t(4)), gt(sub(bs, s), t(5)));
        let white = and(gt(t(6), s), gt(v, t(7)));
        let mut block_bits = or(or(dark, hue), or(chroma, or(white, gt(diff, t(0)))));
        if threshold[13] != 0 {
            block_bits = or(block_bits, and(gt(sub(v, bv), t(8)), invert(gt(s, bs))));
        }
        store_mask(piece_bits, piece.as_mut_ptr().add(x));
        store_mask(block_bits, block.as_mut_ptr().add(x));
        x += 8;
    }
    scalar_row(
        &row[3 * x..],
        bg,
        base,
        table,
        &mut piece[x..],
        &mut block[x..],
    );
}
