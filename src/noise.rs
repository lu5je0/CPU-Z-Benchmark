//! 17.01 SSE noise engine: OpenSimplex2-style 2D noise, bit-identical to
//! CPU-Z's sub_140049FF0 (single sample), sub_140049E50 (gradient lookup),
//! sub_14004A340 (noise A) and sub_14004A540 (noise B).
//!
//! Constants recovered from cpuz_x64.exe:
//!   STRETCH 0.21132487, SQUISH 0.36602542, hash multipliers 127.1/311.7/183.3/269.5.

use crate::table::gradients_1701;

/// floor() via the truncate + fix pattern CPU-Z uses (cvttss2si + comiss fix).
fn floor_f32(v: f32) -> f32 {
    let t = v as i32;
    let tf = t as f32;
    if tf > v {
        (t - 1) as f32
    } else {
        tf
    }
}

/// fract = v - trunc(v), normalized to [0,1) by adding 1 when negative.
fn fract_norm(v: f32) -> f32 {
    let t = v as i32;
    let mut f = v - t as f32;
    if f < 0.0 {
        f += 1.0;
    }
    f
}

/// sub_140049E50: gradient lookup for grid cell (x, y).
/// Returns (gx, gy) in [-1, 1].
#[inline(always)]
pub fn gradient_lookup(x: f32, y: f32) -> (f32, f32) {
    let table = gradients_1701();

    // idx from hash (y*311.7 + x*127.1) * 0.15915495
    let h1 = (x * 127.1 + y * 311.7) * 0.15915495f32;
    let idx1 = (fract_norm(h1) * 1024.0 * 4.0) as i32;
    let t1 = table[idx1 as usize & 0xFFF];

    // idx from hash (y*183.3 + x*269.5) * 0.15915495
    let h2 = (x * 269.5 + y * 183.3) * 0.15915495f32;
    let idx2 = (fract_norm(h2) * 1024.0 * 4.0) as i32;
    let t2 = table[idx2 as usize & 0xFFF];

    let gx = fract_norm(t1 * 43758.547f32) * 2.0 - 1.0;
    let gy = fract_norm(t2 * 43758.547f32) * 2.0 - 1.0;
    (gx, gy)
}

/// sub_140049FF0: single OpenSimplex2 sample at (x, y) in [-1, 1].
#[inline(always)]
pub fn single_sample(x: f32, y: f32) -> f32 {
    // stretch transform
    let squish = (x + y) * 0.36602542f32;
    let xsb = floor_f32(x + squish);
    let ysb = floor_f32(y + squish);
    let stretch = (xsb + ysb) * 0.21132487f32;
    let dx0 = (x - xsb) + stretch;
    let dy0 = (y - ysb) + stretch;

    // pick triangle corner
    let (v11, v12) = if dx0 > dy0 { (1.0f32, 0.0f32) } else { (0.0f32, 1.0f32) };
    let dx1 = (dx0 - v11) + 0.21132487f32;
    let dy1 = (dy0 - v12) + 0.21132487f32;
    let dx2 = (dx0 - 1.0) + 0.42264974f32;
    let dy2 = (dy0 - 1.0) + 0.42264974f32;

    // radial weights (0.5 - d^2), clamped, raised to the 4th
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

    // gradient dot products at the three corners
    let (g0x, g0y) = gradient_lookup(xsb, ysb);
    let dot0 = g0x * dx0 + g0y * dy0;

    let (g1x, g1y) = gradient_lookup(xsb + v11, ysb + v12);
    let dot1 = g1x * dx1 + g1y * dy1;

    let (g2x, g2y) = gradient_lookup(xsb + 1.0, ysb + 1.0);
    let dot2 = g2x * dx2 + g2y * dy2;

    let w0 = w0 * w0 * w0 * w0;
    let w1 = w1 * w1 * w1 * w1;
    let w2 = w2 * w2 * w2 * w2;

    w0 * dot0 + w1 * dot1 + w2 * dot2
}

/// sub_14004A340: 17.01 noise A.
/// Two 10-iteration octave loops with rotation matrix (x',y') =
/// (1.6x - 1.2y, 1.2x + 1.6y); amplitudes 0.8*0.7^k and 0.7*0.6^k, *40.
/// Result: (s1 + s2) * s2 * 8 * s1 + 0.2  where s1 sums |n*w| (abs),
/// s2 sums n*w (no abs).
pub fn noise_a(x: i32, y: i32) -> f32 {
    let xf = (x as f32) * 1.1f32 * 0.0009765625f32;
    let yf = (y as f32) * 1.1f32 * 0.0009765625f32;

    // loop 1: 10 iterations, w starts 0.8, decays *0.7, |n*w| accumulated
    let mut sum1 = 0.0f32;
    let mut w = 0.8f32;
    let mut xx = xf;
    let mut yy = yf;
    for _ in 0..10 {
        let n = single_sample(xx, yy);
        let v = (n * (w * 40.0)).abs();
        sum1 += v;
        w *= 0.7;
        let new_x = xx * 1.6 - yy * 1.2;
        let new_y = xx * 1.2 + yy * 1.6;
        xx = new_x;
        yy = new_y;
    }

    // loop 2: 10 iterations, w starts 0.7, decays *0.6, n*w accumulated (no abs)
    let mut sum2 = 0.0f32;
    let mut w = 0.7f32;
    let mut xx = xf;
    let mut yy = yf;
    for _ in 0..10 {
        let n = single_sample(xx, yy);
        sum2 += n * (w * 40.0);
        w *= 0.6;
        let new_x = xx * 1.6 - yy * 1.2;
        let new_y = xx * 1.2 + yy * 1.6;
        xx = new_x;
        yy = new_y;
    }

    (sum1 + sum2) * sum2 * 8.0 * sum1 + 0.2
}

/// sub_14004A540: 17.01 noise B.
/// Loop 1: 7 iterations, scale 2.2/1024, w starts 0.4 *0.6, n*w summed (no abs).
/// Loop 2: 7 iterations, scale 3.3/1024, w starts 0.4 *0.6, |n*w| summed.
/// Returns sum1 + sum2.
pub fn noise_b(x: i32, y: i32) -> f32 {
    // loop 1
    let xf = (x as f32) * 2.2f32 * 0.0009765625f32;
    let yf = (y as f32) * 2.2f32 * 0.0009765625f32;
    let mut sum1 = 0.0f32;
    let mut w = 0.4f32;
    let mut xx = xf;
    let mut yy = yf;
    for _ in 0..7 {
        let n = single_sample(xx, yy);
        sum1 += n * (w * 40.0);
        w *= 0.6;
        let new_x = xx * 1.6 - yy * 1.2;
        let new_y = xx * 1.2 + yy * 1.6;
        xx = new_x;
        yy = new_y;
    }

    // loop 2
    let xf = (x as f32) * 3.3000002f32 * 0.0009765625f32;
    let yf = (y as f32) * 3.3000002f32 * 0.0009765625f32;
    let mut sum2 = 0.0f32;
    let mut w = 0.4f32;
    let mut xx = xf;
    let mut yy = yf;
    for _ in 0..7 {
        let n = single_sample(xx, yy);
        sum2 += (n * (w * 40.0)).abs();
        w *= 0.6;
        let new_x = xx * 1.6 - yy * 1.2;
        let new_y = xx * 1.2 + yy * 1.6;
        xx = new_x;
        yy = new_y;
    }

    sum1 + sum2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_in_range() {
        for &(x, y) in &[(0, 0), (1, 2), (100, 200), (511, 513), (1023, 1024)] {
            let n = single_sample(x as f32 * 0.0011, y as f32 * 0.0011);
            assert!(n.is_finite(), "x={} y={} n={}", x, y, n);
        }
    }

    #[test]
    fn noise_finite() {
        for &(x, y) in &[(0, 0), (3, 7), (1024, 1024)] {
            assert!(noise_a(x, y).is_finite());
            assert!(noise_b(x, y).is_finite());
        }
    }

    #[test]
    fn gradient_range() {
        let (gx, gy) = gradient_lookup(3.0, 7.0);
        assert!(gx >= -1.0 && gx <= 1.0);
        assert!(gy >= -1.0 && gy <= 1.0);
    }
}
