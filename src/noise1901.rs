//! 19.01 SSE noise engine: single sample (sub_1400312F0), noise A
//! (sub_140031760) and noise B (sub_1400318F0). The 19.01 modes use a
//! pre-transformed gradient table (fract(cos*43758.547)*2-1) and weight
//! tables, unlike 17.01 which transforms table values on the fly.
//!
//! Weight tables recovered from cpuz_x64.exe:
//!   w1 @ 0x1403EFEC4: [~0, 0.8, 0.56, 0.392, 0.2744, 0.19208, 0.134456,
//!                      0.094119, 0.065883, 0.046118]  (0.8*0.7^k)
//!   w2 @ 0x1403EFEEC: [0.0323, 0.7, 0.42, 0.252, 0.1512, 0.09072, 0.054432,
//!                      0.032659, 0.019596, 0.011757] (0.7*0.6^k, first entry unused-ish)
//!   w3 @ 0x1403EFF18: [0.4, 0.144, 0.0864, 0.05184, 0.031104, 0.0186624,
//!                      0.0111974]                     (0.4*0.36^k)

use crate::table::gradients_1901;

/// floor() via truncate + fix (cvttss2si + comiss pattern).
#[inline(always)]
fn floor_f32(v: f32) -> f32 {
    let t = v as i32;
    let tf = t as f32;
    if tf > v {
        (t - 1) as f32
    } else {
        tf
    }
}

/// fract = v - trunc(v), normalized to [0,1) when negative.
#[inline(always)]
fn fract_norm(v: f32) -> f32 {
    let t = v as i32;
    let mut f = v - t as f32;
    if f < 0.0 {
        f += 1.0;
    }
    f
}

/// sub_1400312F0: single OpenSimplex2 sample for 19.01 modes.
/// Gradient values are read directly from the pre-transformed table.
#[inline(always)]
pub fn single_sample(x: f32, y: f32) -> f32 {
    let table = gradients_1901();

    // stretch transform
    let squish = (x + y) * 0.36602542f32;
    let xsb = floor_f32(x + squish);
    let ysb = floor_f32(y + squish);
    let stretch = (xsb + ysb) * 0.21132487f32;
    let dx0 = (x - xsb) + stretch;
    let dy0 = (y - ysb) + stretch;

    // corner selection
    let (v9, v15) = if dx0 > dy0 { (1.0f32, 0.0f32) } else { (0.0f32, 1.0f32) };
    let dx1 = (dx0 - v9) + 0.21132487f32;
    let dy1 = (dy0 - v15) + 0.21132487f32;
    // corner (xsb+1, ysb+1): dx2 = dx0 - 1 + 2*stretch = dx0 - 0.577350259
    let dx2 = dx0 - 0.577350259f32;
    let dy2 = dy0 - 0.577350259f32;

    // weights (0.5 - d^2), clamped, raised to the 4th
    let w0 = {
        let v = 0.5 - (dy0 * dy0 + dx0 * dx0);
        if v < 0.0 { 0.0 } else { v }
    };
    let w1 = {
        let v = 0.5 - (dy1 * dy1 + dx1 * dx1);
        if v < 0.0 { 0.0 } else { v }
    };
    let w2 = {
        let v = 0.5 - (dy2 * dy2 + dx2 * dx2);
        if v < 0.0 { 0.0 } else { v }
    };

    // gradient lookup helper for grid cell (gx, gy)
    let grad = |cx: f32, cy: f32| -> (f32, f32) {
        let h1 = (cx * 127.1 + cy * 311.7) * 0.15915495f32;
        let idx1 = (fract_norm(h1) * 4096.0) as i32;
        let h2 = (cx * 269.5 + cy * 183.3) * 0.15915495f32;
        let idx2 = (fract_norm(h2) * 4096.0) as i32;
        (
            table[idx1 as usize & 0xFFF],
            table[idx2 as usize & 0xFFF],
        )
    };

    let (g0x, g0y) = grad(xsb, ysb);
    let dot0 = g0x * dx0 + g0y * dy0;

    let (g1x, g1y) = grad(xsb + v9, ysb + v15);
    let dot1 = g1x * dx1 + g1y * dy1;

    let (g2x, g2y) = grad(xsb + 1.0, ysb + 1.0);
    let dot2 = g2x * dx2 + g2y * dy2;

    let w0 = w0 * w0 * w0 * w0;
    let w1 = w1 * w1 * w1 * w1;
    let w2 = w2 * w2 * w2 * w2;

    w0 * dot0 + w1 * dot1 + w2 * dot2
}

/// 19.01 weight tables (w1, w2 for noise A; w3 for noise B).
const W1: [f32; 10] = [
    0.0, 0.8, 0.56, 0.392, 0.2744, 0.19208, 0.134456, 0.094119, 0.065883, 0.046118,
];
const W2: [f32; 10] = [
    0.0323, 0.7, 0.42, 0.252, 0.1512, 0.09072, 0.054432, 0.032659, 0.019596, 0.011757,
];
const W3: [f32; 7] = [
    0.4, 0.144, 0.0864, 0.05184, 0.031104, 0.0186624, 0.0111974,
];

/// sub_140031760: 19.01 noise A.
/// 10 iterations, scale 1.1/1024, rotation (x',y') = (1.6x - 1.2y, 1.2x + 1.6y).
/// sum1 += n*40*w2[k]; sum2 += |n*40*w1[k]|.
/// Result: (sum1 + sum2) * sum1 * 8 * sum2 + 0.2
pub fn noise_a(x: i32, y: i32) -> f32 {
    let mut xf = (x as f32) * 1.1f32 * 0.0009765625f32;
    let mut yf = (y as f32) * 1.1f32 * 0.0009765625f32;

    let mut sum1 = 0.0f32;
    let mut sum2 = 0.0f32;
    for k in 0..10 {
        let n = single_sample(xf, yf);
        sum1 += n * 40.0 * W2[k];
        sum2 += (n * 40.0 * W1[k]).abs();

        let new_x = xf * 1.6 - yf * 1.2;
        let new_y = xf * 1.2 + yf * 1.6;
        xf = new_x;
        yf = new_y;
    }

    (sum1 + sum2) * sum1 * 8.0 * sum2 + 0.2
}

/// sub_1400318F0: 19.01 noise B.
/// 7 iterations, scale 2.2/1024, rotation (x',y') = (1.6x - 1.2y, 1.2x + 1.6y).
/// sum += |n*40*w3[k]| + n*40*w3[k]  (i.e. 2*max(v,0) with v = n*40*w3[k]).
pub fn noise_b(x: i32, y: i32) -> f32 {
    let mut xf = (x as f32) * 2.2f32 * 0.0009765625f32;
    let mut yf = (y as f32) * 2.2f32 * 0.0009765625f32;

    let mut sum = 0.0f32;
    for k in 0..7 {
        let n = single_sample(xf, yf);
        let v = n * 40.0 * W3[k];
        sum += v.abs() + v;

        let new_x = xf * 1.6 - yf * 1.2;
        let new_y = xf * 1.2 + yf * 1.6;
        xf = new_x;
        yf = new_y;
    }

    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite() {
        for &(x, y) in &[(0, 0), (3, 7), (1024, 1024)] {
            assert!(noise_a(x, y).is_finite());
            assert!(noise_b(x, y).is_finite());
        }
    }
}
