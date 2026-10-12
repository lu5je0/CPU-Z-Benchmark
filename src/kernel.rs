//! Per-pixel render kernel: two noise samples + color mixing, byte-identical
//! to CPU-Z's sub_14004A750 (17.01) / sub_140030570 (19.01).
//!
//! Pixel layout is RGBA: R = c1*255, G = c2*255, B = c3*255, A = 0xFF.
//!
//! Mixing math (from disassembly):
//!   n1 = noiseA(x, y), n2 = noiseB(x, y)
//!   v15 = clamp(n2 * 0.6 + 0.5, 0, 1)
//!   v16 = clamp(n1 + n2, 0, 1)
//!   v20 = clamp(v15 + 0.1, 0, 1)
//!   v21 = clamp(v15 + 0.2, 0, 1)
//!   v24 = clamp(v15 * 0.9 + 0.3, 0, 1)
//!   c1  = v20 * v16 + (1 - v16) * 0.2
//!   c2  = v21 * v16 + (1 - v16) * 0.4
//!   c3  = v24 * v16 + (1 - v16) * 0.6
//!
//! Coordinate convention (from worker disassembly):
//!   sample x = x0 + i, sample y = y0 + row, buffer row = row.
//! CPU-Z writes pixel data into buffer row `row` (sub_14004A750 uses
//! v11 = buffer + 4*row*width + 2), while noise is sampled at
//! (x0 + i, y0 + row).

/// Which noise engine to use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    /// 17.01 SSE (default benchmark version)
    V1701,
    /// 19.01 SSE
    V1901Sse,
    /// 19.01 AVX2 (8 lanes)
    V1901Avx2,
}

impl Engine {
    pub fn name(&self) -> &'static str {
        match self {
            Engine::V1701 => "17.01",
            Engine::V1901Sse => "19.01",
            Engine::V1901Avx2 => "19.01-AVX2",
        }
    }

    /// Whether the score formula divides by 10 (all 19.01 variants).
    pub fn div10(&self) -> bool {
        matches!(self, Engine::V1901Sse | Engine::V1901Avx2)
    }
}

#[inline(always)]
fn clamp01(v: f32) -> f32 {
    if v < 0.0 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

/// Render one pixel. x, y are sample coordinates.
#[inline(always)]
pub fn pixel(engine: Engine, x: i32, y: i32) -> [u8; 4] {
    let (n1, n2) = match engine {
        Engine::V1701 => (
            crate::noise::noise_a(x, y),
            crate::noise::noise_b(x, y),
        ),
        Engine::V1901Sse => (
            crate::noise1901::noise_a(x, y),
            crate::noise1901::noise_b(x, y),
        ),
        #[cfg(target_arch = "x86_64")]
        Engine::V1901Avx2 => unsafe {
            (
                crate::noise_avx2::noise_a(x, y),
                crate::noise_avx2::noise_b(x, y),
            )
        },
        // No AVX2 on this architecture: use the 19.01 SSE engine instead.
        #[cfg(not(target_arch = "x86_64"))]
        Engine::V1901Avx2 => (
            crate::noise1901::noise_a(x, y),
            crate::noise1901::noise_b(x, y),
        ),
    };

    let v15 = clamp01(n2 * 0.60000002 + 0.5);
    let v16 = clamp01(n1 + n2);
    let v20 = clamp01(v15 + 0.1);
    let v21 = clamp01(v15 + 0.2);
    let v24 = clamp01(v15 * 0.89999998 + 0.30000001);

    let inv = 1.0 - v16;
    let c1 = v20 * v16 + inv * 0.2;
    let c2 = v21 * v16 + inv * 0.40000001;
    let c3 = v24 * v16 + inv * 0.60000002;

    [
        (c1 * 255.0) as u8,
        (c2 * 255.0) as u8,
        (c3 * 255.0) as u8,
        0xFF,
    ]
}

/// Render `rows` horizontal lines into buf.
///
/// - Sample coordinates: x in [x0, x0+width), y = y0 + (row0 + r).
/// - Buffer: each row r is written at offset (row0 + r) * row_stride.
#[inline(always)]
pub fn render_rows(
    engine: Engine,
    buf: &mut [u8],
    x0: i32,
    y0: i32,
    row0: i32,
    rows: usize,
    width: usize,
    row_stride: usize,
) {
    // Fast path: use raw pointer walk like CPU-Z (v11 += 4 per pixel).
    // SAFETY: caller guarantees rows*row_stride fits in buf, and we iterate
    // within [0, rows) rows and [0, width) pixels.
    let base = buf.as_mut_ptr();
    for r in 0..rows {
        let row = row0 + r as i32;
        let p = unsafe { base.add((row as usize) * row_stride) };
        let sy = y0 + row;
        for i in 0..width {
            let px = pixel(engine, x0 + i as i32, sy);
            unsafe {
                let q = p.add(i * 4);
                *q = px[0];
                *q.add(1) = px[1];
                *q.add(2) = px[2];
                *q.add(3) = px[3];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_always_ff() {
        for e in [Engine::V1701, Engine::V1901Sse, Engine::V1901Avx2] {
            for &(x, y) in &[(0, 0), (1, 1), (1023, 1023)] {
                assert_eq!(pixel(e, x, y)[3], 0xFF);
            }
        }
    }
}
