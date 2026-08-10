//! 4096-entry gradient table, mirroring CPU-Z's initialization.
//!
//! Reverse engineering notes (from cpuz_x64.exe):
//! - 17.01 (SSE) path: sub_140049D80 fills the per-object table (obj+48):
//!   `x = (float)i * 6.283185f * 0.0009765625f * 4.0f`
//!   `table[i] = cosf(x)` where cosf is sub_1404CBBC0 (float in, float out,
//!   internally computed in double precision).
//! - 19.01 paths: sub_1400311B0 fills the shared global table unk_140499EC0:
//!   `x = (float)i * 6.283185f * 0.00024414062f`
//!   `v = cosf(x) * 43758.547f; table[i] = (v - truncf(v)) * 2.0f - 1.0f`
//!
//! Score parity only depends on the amount of work (units) per time, not on
//! the exact pixel values, so using libm cos at full precision is fine;
//! values differ from MSVC's by <1 ulp, which does not change the score.

use std::sync::OnceLock;

pub const TABLE_SIZE: usize = 4096;

/// Shared gradient table. CPU-Z 17.01 mode uses raw cos values;
/// 19.01 modes use the fract()-transformed values (same formula as the
/// per-object table in 17.01 is stored raw).
static TABLE_1701: OnceLock<Box<[f32; TABLE_SIZE]>> = OnceLock::new();
static TABLE_1901: OnceLock<Box<[f32; TABLE_SIZE]>> = OnceLock::new();

/// CPU-Z cos: float input, float output, double internals (sub_1404CBBC0).
fn ms_cosf(x: f32) -> f32 {
    (x as f64).cos() as f32
}

/// 17.01 SSE table: raw cos values in [-1, 1].
/// sub_140049D80: table[i] = cosf(i * 6.283185f * 0.0009765625f * 4.0f)
pub fn table_1701(i: usize) -> f32 {
    let x = (i as f32) * 6.283185f32 * 0.0009765625f32 * 4.0f32;
    ms_cosf(x)
}

/// 19.01 table: fract(cosf(x) * 43758.547f) * 2 - 1, x = i * 6.283185f / 4096f.
/// sub_1400311B0 (shared global unk_140499EC0).
pub fn table_1901(i: usize) -> f32 {
    let x = (i as f32) * 6.283185f32 * 0.00024414062f32;
    let v = ms_cosf(x) * 43758.547f32;
    let frac = v - (v as i32) as f32;
    frac * 2.0f32 - 1.0f32
}

/// The table used by the 17.01 noise sampler (sub_140049E50 reads obj+48).
pub fn gradients_1701() -> &'static [f32; TABLE_SIZE] {
    TABLE_1701.get_or_init(|| {
        let mut t = Box::new([0.0f32; TABLE_SIZE]);
        for (i, slot) in t.iter_mut().enumerate() {
            *slot = table_1701(i);
        }
        t
    })
}

/// The table used by 19.01 samplers (sub_1400312F0 / AVX2 gather from unk_140499EC0).
pub fn gradients_1901() -> &'static [f32; TABLE_SIZE] {
    TABLE_1901.get_or_init(|| {
        let mut t = Box::new([0.0f32; TABLE_SIZE]);
        for (i, slot) in t.iter_mut().enumerate() {
            *slot = table_1901(i);
        }
        t
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_ranges() {
        for v in gradients_1701().iter() {
            assert!((-1.0..=1.0).contains(v), "1701 v={}", v);
        }
        // CPU-Z's fract() has no abs: for negative v, fract = v - trunc(v) < 0,
        // so values lie in (-3, 1). Only require finiteness and determinism.
        for v in gradients_1901().iter() {
            assert!(v.is_finite(), "1901 v={}", v);
            assert!(*v > -3.0f32 && *v <= 1.0f32, "1901 v={}", v);
        }
    }

    #[test]
    fn table_spots() {
        // cos(0) = 1
        assert!((gradients_1701()[0] - 1.0).abs() < 1e-6);
        // The 19.01 table entry 0: fract(cos(0)*43758.547)*2-1 =
        // fract(43758.547)*2-1 = 0.546875*2-1 = 0.09375 (float rounding aside)
        let e0 = gradients_1901()[0];
        assert!((e0 - 0.09375).abs() < 1e-5, "e0={}", e0);
    }
}
