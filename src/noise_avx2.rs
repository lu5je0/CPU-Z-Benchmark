//! 19.01 AVX2 noise engine — true 8-lane SIMD implementation.
//!
//! Ported from CPU-Z's sub_14032A8D0 (noise A) and sub_14032AFA0 (noise B).
//! The OpenSimplex2 sample is computed on 8 lanes at once with AVX2
//! intrinsics, matching CPU-Z's throughput so the score (units/ms) lines up.
//!
//! Key facts from disassembly:
//! - noise A: 10 rotated points; group 1 = points 0..7 (8 lanes), group 2 =
//!   points 8,9 + junk (only lanes 0,1 have non-zero weights W_A2/W_A3).
//!   Final: (s1 + s2) * s1 * 8 * s2 + 0.2
//!   s1 = sum(r1*W_A1 + r2*W_A2), s2 = sum(|r2*W_A3| + |r1*W_A4|)
//! - noise B: 7 rotated points + junk lane, scale 2.2/1024.
//!   Final: sum(|r*W_B| + r*W_B)
//! - gradient table read via vgatherdps.
//!
//! The sample body is a macro so it fully inlines into the 8-lane loops —
//! `#[target_feature]` functions cannot inline into each other by default
//! (call overhead killed performance, see earlier commits).

use std::arch::x86_64::*;

use crate::table::gradients_1901;

const W_A1: [f32; 8] = [
    f32::from_bits(0x41e0_0000), // 28
    f32::from_bits(0x4186_6667), // 16.8
    f32::from_bits(0x4121_47af), // 10.08
    f32::from_bits(0x40c1_8939), // 6.048
    f32::from_bits(0x4068_3e45), // 3.6288
    f32::from_bits(0x400b_5890), // 2.17728
    f32::from_bits(0x3fa7_3714), // 1.3063684
    f32::from_bits(0x3f48_a87f), // 0.78382105
];
const W_A2: [f32; 8] = [
    f32::from_bits(0x3ef0_ca33), // 0.47029266
    f32::from_bits(0x3e90_7952), // 0.2821756
    0.0,
    0.0,
    0.0,
    0.0,
    0.0,
    0.0,
];
const W_A3: [f32; 8] = [
    f32::from_bits(0x3fec_2050), // 1.8447361
    f32::from_bits(0x3fa5_49d1), // 1.2913152
    0.0,
    0.0,
    0.0,
    0.0,
    0.0,
    0.0,
];
const W_A4: [f32; 8] = [
    f32::from_bits(0x4200_0000), // 32
    f32::from_bits(0x41b3_3333), // 22.4
    f32::from_bits(0x417a_e147), // 15.68
    f32::from_bits(0x412f_9db2), // 10.976
    f32::from_bits(0x40f5_dcc6), // 7.6832
    f32::from_bits(0x40ac_1a8a), // 5.3782396
    f32::from_bits(0x4070_f1f4), // 3.7647676
    f32::from_bits(0x4028_a95e), // 2.6353374
];
const W_B: [f32; 8] = [
    f32::from_bits(0x4180_0000), // 16
    f32::from_bits(0x40b8_51ec), // 5.76
    f32::from_bits(0x405d_2f1c), // 3.456
    f32::from_bits(0x4004_b5de), // 2.0736
    f32::from_bits(0x3f9f_40a4), // 1.24416
    f32::from_bits(0x3f3f_1a5f), // 0.74649614
    f32::from_bits(0x3ee5_52d9), // 0.4478977
    0.0,
];

#[inline(always)]
unsafe fn load_w(w: &[f32; 8]) -> __m256 {
    _mm256_loadu_ps(w.as_ptr())
}

/// 8-lane OpenSimplex2 sample body. Expands `$out` to the 8 result lanes.
macro_rules! sample8 {
    ($xs:expr, $ys:expr, $out:ident) => {
        {
            let xs: __m256 = $xs;
            let ys: __m256 = $ys;
            let squish = _mm256_mul_ps(_mm256_add_ps(xs, ys), _mm256_set1_ps(0.36602542));
            // floor(x + squish)
            let xs_p = _mm256_add_ps(xs, squish);
            let xsb_t = _mm256_cvttps_epi32(xs_p);
            let xsb_f = _mm256_cvtepi32_ps(xsb_t);
            let fix_x = _mm256_cmp_ps(xsb_f, xs_p, _CMP_GT_OS);
            let xsb = _mm256_cvtepi32_ps(_mm256_sub_epi32(
                xsb_t,
                _mm256_cvttps_epi32(_mm256_and_ps(fix_x, _mm256_set1_ps(-1.0))),
            ));
            let ys_p = _mm256_add_ps(ys, squish);
            let ysb_t = _mm256_cvttps_epi32(ys_p);
            let ysb_f = _mm256_cvtepi32_ps(ysb_t);
            let fix_y = _mm256_cmp_ps(ysb_f, ys_p, _CMP_GT_OS);
            let ysb = _mm256_cvtepi32_ps(_mm256_sub_epi32(
                ysb_t,
                _mm256_cvttps_epi32(_mm256_and_ps(fix_y, _mm256_set1_ps(-1.0))),
            ));

            let stretch = _mm256_mul_ps(_mm256_add_ps(xsb, ysb), _mm256_set1_ps(0.21132487));
            let dx0 = _mm256_add_ps(_mm256_sub_ps(xs, xsb), stretch);
            let dy0 = _mm256_add_ps(_mm256_sub_ps(ys, ysb), stretch);

            // corner selection: v9 = dx0 > dy0 ? 1 : 0
            let gt = _mm256_cmp_ps(dx0, dy0, _CMP_GT_OS);
            let v9 = _mm256_and_ps(gt, _mm256_set1_ps(1.0));
            let v15 = _mm256_sub_ps(_mm256_set1_ps(1.0), v9);

            let dx1 = _mm256_add_ps(_mm256_sub_ps(dx0, v9), _mm256_set1_ps(0.21132487));
            let dy1 = _mm256_add_ps(_mm256_sub_ps(dy0, v15), _mm256_set1_ps(0.21132487));
            let dx2 = _mm256_sub_ps(dx0, _mm256_set1_ps(0.577350259));
            let dy2 = _mm256_sub_ps(dy0, _mm256_set1_ps(0.577350259));

            let zero = _mm256_setzero_ps();
            let half = _mm256_set1_ps(0.5);
            let w0 = _mm256_max_ps(
                _mm256_sub_ps(half, _mm256_add_ps(_mm256_mul_ps(dx0, dx0), _mm256_mul_ps(dy0, dy0))),
                zero,
            );
            let w1 = _mm256_max_ps(
                _mm256_sub_ps(half, _mm256_add_ps(_mm256_mul_ps(dx1, dx1), _mm256_mul_ps(dy1, dy1))),
                zero,
            );
            let w2 = _mm256_max_ps(
                _mm256_sub_ps(half, _mm256_add_ps(_mm256_mul_ps(dx2, dx2), _mm256_mul_ps(dy2, dy2))),
                zero,
            );

            let table = gradients_1901();
            // gradient lookup: two hashes per corner
            let grad = |cx: __m256, cy: __m256| -> (__m256, __m256) {
                let h1 = _mm256_add_ps(
                    _mm256_mul_ps(cx, _mm256_set1_ps(29.173103)),
                    _mm256_mul_ps(cy, _mm256_set1_ps(42.892258)),
                );
                // fract_norm
                let h1_t = _mm256_cvttps_epi32(h1);
                let h1_f = _mm256_cvtepi32_ps(h1_t);
                let mut f1 = _mm256_sub_ps(h1, h1_f);
                let neg1 = _mm256_cmp_ps(f1, zero, _CMP_LT_OS);
                f1 = _mm256_add_ps(f1, _mm256_and_ps(neg1, _mm256_set1_ps(1.0)));
                let idx1 = _mm256_cvttps_epi32(_mm256_mul_ps(f1, _mm256_set1_ps(4096.0)));

                let h2 = _mm256_add_ps(
                    _mm256_mul_ps(cx, _mm256_set1_ps(49.608601)),
                    _mm256_mul_ps(cy, _mm256_set1_ps(20.228594)),
                );
                let h2_t = _mm256_cvttps_epi32(h2);
                let h2_f = _mm256_cvtepi32_ps(h2_t);
                let mut f2 = _mm256_sub_ps(h2, h2_f);
                let neg2 = _mm256_cmp_ps(f2, zero, _CMP_LT_OS);
                f2 = _mm256_add_ps(f2, _mm256_and_ps(neg2, _mm256_set1_ps(1.0)));
                let idx2 = _mm256_cvttps_epi32(_mm256_mul_ps(f2, _mm256_set1_ps(4096.0)));

                let gx = _mm256_i32gather_ps(table.as_ptr(), idx1, 4);
                let gy = _mm256_i32gather_ps(table.as_ptr(), idx2, 4);
                (gx, gy)
            };

            let (g0x, g0y) = grad(xsb, ysb);
            let dot0 = _mm256_add_ps(_mm256_mul_ps(g0x, dx0), _mm256_mul_ps(g0y, dy0));

            let (g1x, g1y) = grad(_mm256_add_ps(xsb, v9), _mm256_add_ps(ysb, v15));
            let dot1 = _mm256_add_ps(_mm256_mul_ps(g1x, dx1), _mm256_mul_ps(g1y, dy1));

            let (g2x, g2y) = grad(
                _mm256_add_ps(xsb, _mm256_set1_ps(1.0)),
                _mm256_add_ps(ysb, _mm256_set1_ps(1.0)),
            );
            let dot2 = _mm256_add_ps(_mm256_mul_ps(g2x, dx2), _mm256_mul_ps(g2y, dy2));

            let w0 = _mm256_mul_ps(_mm256_mul_ps(w0, w0), _mm256_mul_ps(w0, w0));
            let w1 = _mm256_mul_ps(_mm256_mul_ps(w1, w1), _mm256_mul_ps(w1, w1));
            let w2 = _mm256_mul_ps(_mm256_mul_ps(w2, w2), _mm256_mul_ps(w2, w2));

            let mut acc = _mm256_mul_ps(w0, dot0);
            acc = _mm256_add_ps(acc, _mm256_mul_ps(w1, dot1));
            acc = _mm256_add_ps(acc, _mm256_mul_ps(w2, dot2));
            $out = acc;
        }
    };
}

/// Horizontal sum of a __m256 (vperm2f128 + vaddps + vhaddps pattern).
#[inline(always)]
unsafe fn hsum(v: __m256) -> f32 {
    let hi = _mm256_permute2f128_ps(v, v, 1);
    let lo = _mm256_add_ps(v, hi);
    let lo128 = _mm256_castps256_ps128(lo);
    let sum = _mm_hadd_ps(lo128, lo128);
    let sum = _mm_hadd_ps(sum, sum);
    _mm_cvtss_f32(sum)
}

/// 8-lane render for the AVX2 engine, inlined into the AVX2 context.
#[target_feature(enable = "avx2")]
pub unsafe fn render_rows_avx2(
    buf: &mut [u8],
    x0: i32,
    y0: i32,
    row0: i32,
    rows: usize,
    width: usize,
    row_stride: usize,
) {
    for r in 0..rows {
        let row = row0 + r as i32;
        let base = row as usize * row_stride;
        let sy = y0 + row;
        for i in 0..width {
            let x = x0 + i as i32;
            let n1 = noise_a(x, sy);
            let n2 = noise_b(x, sy);
            let v15 = {
                let v = n2 * 0.60000002 + 0.5;
                if v < 0.0 { 0.0 } else if v > 1.0 { 1.0 } else { v }
            };
            let v16 = {
                let v = n1 + n2;
                if v < 0.0 { 0.0 } else if v > 1.0 { 1.0 } else { v }
            };
            let v20 = {
                let v = v15 + 0.1;
                if v < 0.0 { 0.0 } else if v > 1.0 { 1.0 } else { v }
            };
            let v21 = {
                let v = v15 + 0.2;
                if v < 0.0 { 0.0 } else if v > 1.0 { 1.0 } else { v }
            };
            let v24 = {
                let v = v15 * 0.89999998 + 0.30000001;
                if v < 0.0 { 0.0 } else if v > 1.0 { 1.0 } else { v }
            };
            let inv = 1.0 - v16;
            let c1 = v20 * v16 + inv * 0.2;
            let c2 = v21 * v16 + inv * 0.40000001;
            let c3 = v24 * v16 + inv * 0.60000002;
            let off = base + i * 4;
            buf[off] = (c1 * 255.0) as u8;
            buf[off + 1] = (c2 * 255.0) as u8;
            buf[off + 2] = (c3 * 255.0) as u8;
            buf[off + 3] = 0xFF;        }
    }
}

/// sub_14032A8D0: 19.01 AVX2 noise A (single pixel, 8-lane internals).
#[target_feature(enable = "avx2")]
#[inline]
pub unsafe fn noise_a(x: i32, y: i32) -> f32 {
    // 10 rotated points
    let mut pts = [(0.0f32, 0.0f32); 10];
    let mut px = (x as f32) * 0.0010742188f32;
    let mut py = (y as f32) * 0.0010742188f32;
    for p in pts.iter_mut() {
        *p = (px, py);
        let nx = px * 1.6 - py * 1.2;
        let ny = px * 1.2 + py * 1.6;
        px = nx;
        py = ny;
    }

    // group 1: points 0..7
    let mut xs1 = [0.0f32; 8];
    let mut ys1 = [0.0f32; 8];
    for i in 0..8 {
        xs1[i] = pts[i].0;
        ys1[i] = pts[i].1;
    }
    let mut r1 = _mm256_setzero_ps();
    sample8!(
        _mm256_loadu_ps(xs1.as_ptr()),
        _mm256_loadu_ps(ys1.as_ptr()),
        r1
    );

    // group 2: points 8,9 (+ junk, weights 0)
    let mut xs2 = [0.0f32; 8];
    let mut ys2 = [0.0f32; 8];
    for i in 0..8 {
        let (px, py) = if i < 2 { pts[8 + i] } else { pts[8 + (i & 1)] };
        xs2[i] = px;
        ys2[i] = py;
    }
    let mut r2 = _mm256_setzero_ps();
    sample8!(
        _mm256_loadu_ps(xs2.as_ptr()),
        _mm256_loadu_ps(ys2.as_ptr()),
        r2
    );

    let w_a1 = load_w(&W_A1);
    let w_a2 = load_w(&W_A2);
    let w_a3 = load_w(&W_A3);
    let w_a4 = load_w(&W_A4);

    let s1v = _mm256_add_ps(_mm256_mul_ps(r1, w_a1), _mm256_mul_ps(r2, w_a2));
    let s1 = hsum(s1v);

    let s2v = _mm256_add_ps(
        _mm256_andnot_ps(_mm256_set1_ps(-0.0), _mm256_mul_ps(r2, w_a3)),
        _mm256_andnot_ps(_mm256_set1_ps(-0.0), _mm256_mul_ps(r1, w_a4)),
    );
    let s2 = hsum(s2v);

    (s1 + s2) * s1 * 8.0 * s2 + 0.2
}

/// sub_14032AFA0: 19.01 AVX2 noise B (single pixel, 8-lane internals).
#[target_feature(enable = "avx2")]
#[inline]
pub unsafe fn noise_b(x: i32, y: i32) -> f32 {
    // 7 rotated points, scale 2.2/1024
    let mut pts = [(0.0f32, 0.0f32); 7];
    let mut px = (x as f32) * 0.0021484375f32;
    let mut py = (y as f32) * 0.0021484375f32;
    for p in pts.iter_mut() {
        *p = (px, py);
        let nx = px * 1.6 - py * 1.2;
        let ny = px * 1.2 + py * 1.6;
        px = nx;
        py = ny;
    }

    let mut xs = [0.0f32; 8];
    let mut ys = [0.0f32; 8];
    for i in 0..8 {
        let (px, py) = if i < 7 { pts[i] } else { pts[6] };
        xs[i] = px;
        ys[i] = py;
    }
    let mut r = _mm256_setzero_ps();
    sample8!(_mm256_loadu_ps(xs.as_ptr()), _mm256_loadu_ps(ys.as_ptr()), r);

    let w_b = load_w(&W_B);
    let v = _mm256_mul_ps(r, w_b);
    let abs = _mm256_andnot_ps(_mm256_set1_ps(-0.0), v);
    let s = hsum(_mm256_add_ps(abs, v));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_avx2() {
        if std::is_x86_feature_detected!("avx2") {
            unsafe {
                for &(x, y) in &[(0, 0), (3, 7), (1024, 1024)] {
                    let a = noise_a(x, y);
                    let b = noise_b(x, y);
                    assert!(a.is_finite(), "A({},{})={}", x, y, a);
                    assert!(b.is_finite(), "B({},{})={}", x, y, b);
                }
            }
        }
    }
}
