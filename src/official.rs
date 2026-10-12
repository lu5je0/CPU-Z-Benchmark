//! Run CPU-Z's own Windows-ARM64 benchmark kernel natively on this machine.
//!
//! CPUID ships `cpuz_arm64.exe` compiled with MSVC. On Apple Silicon that build
//! runs measurably slower than this crate's LLVM build of the *same* algorithm
//! (see AGENTS.md, "ARM64 fidelity"), so our own number is not the number
//! CPU-Z reports. The kernel itself is pure scalar AArch64 with no imports, no
//! vector-register FP and no external data beyond the 4096-float gradient table
//! we pass in, which means we can map the user's own copy of `cpuz_arm64.exe`
//! and call the kernel directly.
//!
//! Nothing here bundles or redistributes the executable: the user supplies it
//! via `--official <path>`, `CPUZ_ARM64_EXE`, or the shared cache path that the
//! `q-cpuz-benchmark` wrapper fills in.
//!
//! Layout, recovered from the v1.05 and v1.06 disassemblies and verified by
//! executing both natively:
//!
//! * the 16-float constant pool (`1.1, 1/1024, 2.2, 3.3, 40, 1.2, 1.6, 0.6,
//!   0.8, 0.7, 0.4, 0.2, 0.1, 0.9, 0.3, 255`) sits 0x360 bytes after the tile
//!   function, in both builds;
//! * the tile function renders exactly one row and returns the row's pixel
//!   count in its work counter, so a 1024x1024 frame reports 1048576 units and
//!   the score unit is pixels/ms;
//! * the parameter block it reads is `+0x18 ctx, +0x20 dst, +0x28 x0, +0x2c y0,
//!   +0x30 width, +0x34 row, +0x3c work(out)`, and the context holds the
//!   gradient table pointer at `+0x38`.
//!
//! Everything is PC-relative inside the mapped image, so mapping the file
//! contiguously at an arbitrary base keeps every internal call and literal
//! pool load valid -- no PE relocation or loader is needed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use crate::bench::{BenchOptions, BenchResult, Pass, PassResult, Progress};

pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 1024;

/// Constant pool that identifies the 17.01 tile function.
const POOL: [f32; 16] = [
    1.1,
    0.0009765625,
    2.2,
    3.3000001907348633,
    40.0,
    1.2,
    1.6,
    0.6,
    0.8,
    0.7,
    0.4,
    0.2,
    0.1,
    0.9,
    0.3,
    255.0,
];

/// Distance in bytes from the start of the pool back to the tile function.
const TILE_FROM_POOL: usize = 0x360;

/// First four instructions of the tile function, used to confirm the guess.
const TILE_PROLOGUE: [u8; 16] = [
    0xff, 0x03, 0x01, 0xd1, // sub  sp, sp, #0x40
    0xfd, 0x7b, 0xbd, 0xa9, // stp  x29, x30, [sp, #-0x30]!
    0xfd, 0x03, 0x00, 0x91, // mov  x29, sp
    0xe3, 0x03, 0x01, 0xaa, // mov  x3, x1
];

#[repr(C)]
struct Ctx {
    pad: [u8; 0x38],
    tab: *const f32,
}

// `tab` points at the shared, immutable gradient table.
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

#[repr(C)]
struct Param {
    pad0: [u8; 0x18],
    ctx: *const Ctx,
    dst: *mut u8,
    x0: i32,
    y0: i32,
    width: i32,
    row: i32,
    unk38: i32,
    work: u32,
    active: u32,
    pad1: [u8; 4],
}

type TileFn = unsafe extern "C" fn(*mut u8, *mut Param);

/// A mapped, executable copy of the official kernel.
pub struct Kernel {
    code: *mut u8,
    len: usize,
    tile: TileFn,
    pub path: PathBuf,
    pub version: String,
}

// The mapping is immutable and executable once loaded; the kernel itself is
// pure (only touches the parameter block and the caller's buffer).
unsafe impl Send for Kernel {}
unsafe impl Sync for Kernel {}

impl Drop for Kernel {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.code as *mut libc::c_void, self.len);
        }
    }
}

impl Kernel {
    pub fn load(path: &Path) -> Result<Kernel, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if bytes.len() < 0x1000 {
            return Err(format!("{}: too small to be cpuz_arm64.exe", path.display()));
        }

        let mut sig = [0u8; 64];
        for (i, v) in POOL.iter().enumerate() {
            sig[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut tile_off = None;
        for i in TILE_FROM_POOL..bytes.len().saturating_sub(64) {
            let e = i + 64;
            if bytes[i..e] == sig {
                let t = i - TILE_FROM_POOL;
                if bytes[t..t + 16] == TILE_PROLOGUE {
                    tile_off = Some(t);
                    break;
                }
            }
        }
        let tile_off = tile_off.ok_or_else(|| {
            format!(
                "{}: 17.01 kernel signature not found (unsupported CPU-Z ARM64 build?)",
                path.display()
            )
        })?;

        let len = bytes.len();
        let code = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if code == libc::MAP_FAILED {
            return Err("mmap failed".to_string());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), code as *mut u8, len);
            if libc::mprotect(code, len, libc::PROT_READ | libc::PROT_EXEC) != 0 {
                libc::munmap(code, len);
                return Err("mprotect(PROT_EXEC) failed".to_string());
            }
        }
        let tile: TileFn = unsafe { std::mem::transmute((code as *mut u8).add(tile_off)) };

        Ok(Kernel {
            code: code as *mut u8,
            len,
            tile,
            path: path.to_path_buf(),
            version: build_label(len),
        })
    }

    fn run_pass(
        &self,
        pass: Pass,
        opts: &BenchOptions,
        threads: usize,
        on_progress: &mut dyn FnMut(Progress),
    ) -> PassResult {
        let table = crate::table::gradients_1701();
        let ctx = Ctx {
            pad: [0; 0x38],
            tab: table.as_ptr(),
        };
        let mut buf = vec![0u8; WIDTH * HEIGHT * 4];

        let next = AtomicU64::new(0);
        let units = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let dst = buf.as_mut_ptr() as usize;
        let height = HEIGHT as u64;
        let threads = threads.max(1);

        let start = Instant::now();
        let duration = opts.duration_ms;

        std::thread::scope(|scope| {
            for _ in 0..threads {
                let (next, units, stop, ctx, tile) = (&next, &units, &stop, &ctx, self.tile);
                scope.spawn(move || {
                    let mut param = Param {
                        pad0: [0; 0x18],
                        ctx,
                        dst: dst as *mut u8,
                        x0: 0,
                        y0: 0,
                        width: WIDTH as i32,
                        row: 0,
                        unk38: 0,
                        work: 0,
                        active: 0,
                        pad1: [0; 4],
                    };
                    while !stop.load(Ordering::Relaxed) {
                        let idx = next.fetch_add(1, Ordering::Relaxed);
                        let frame = (idx / height) as i32;
                        let row = (idx % height) as i32;
                        param.x0 = frame;
                        param.y0 = frame;
                        param.row = row;
                        param.work = 0;
                        unsafe { tile(std::ptr::null_mut(), &mut param) };
                        units.fetch_add(param.work as u64, Ordering::Relaxed);
                    }
                });
            }

            // Main thread drives the clock and the live panel.
            loop {
                std::thread::sleep(std::time::Duration::from_millis(
                    opts.report_interval_ms.max(20),
                ));
                let elapsed = start.elapsed().as_millis() as u64;
                let u = units.load(Ordering::Relaxed);
                let done = elapsed >= duration;
                let score = if elapsed == 0 {
                    0.0
                } else {
                    u as f32 / elapsed as f32
                };
                if opts.report_interval_ms > 0 {
                    on_progress(Progress {
                        pass,
                        done,
                        score,
                        units: u,
                        elapsed_ms: elapsed.min(duration),
                        duration_ms: duration,
                        threads,
                    });
                }
                if done {
                    break;
                }
            }
            stop.store(true, Ordering::Relaxed);
        });

        let ms = start.elapsed().as_millis() as u64;
        let u = units.load(Ordering::Relaxed);
        let result = PassResult {
            score: u as f32 / ms.max(1) as f32,
            units: u,
            ms,
            threads,
        };
        if opts.report_interval_ms > 0 {
            on_progress(Progress {
                pass,
                done: true,
                score: result.score,
                units: result.units,
                elapsed_ms: ms,
                duration_ms: duration,
                threads,
            });
        }
        result
    }
}

/// Identify the build from its size. The exe carries no plain version string
/// and the resources are UTF-16, so size is the simplest reliable marker.
fn build_label(len: usize) -> String {
    match len {
        5_203_688 => "v1.05".to_string(),
        6_914_552 => "v1.06".to_string(),
        n => format!("(unrecognised build, {n} bytes)"),
    }
}

/// Cache location shared with the `q-cpuz-benchmark` wrapper.
pub fn default_cache_path() -> PathBuf {
    let base = std::env::var_os("CPUZ_BENCHMARK_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".cache")
        });
    base.join("cpuz-benchmark").join("cpuz_arm64.exe")
}

/// Where the official executable is, if we can find it.
pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("CPUZ_ARM64_EXE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let cache = default_cache_path();
    if cache.is_file() {
        return Some(cache);
    }
    None
}

/// Run the official kernel's single- and multi-thread passes.
pub fn run<F: FnMut(Progress)>(
    kernel: &Kernel,
    opts: &BenchOptions,
    on_progress: &mut F,
) -> BenchResult {
    let mut sink = |p: Progress| on_progress(p);
    let single = if opts.run_single {
        Some(kernel.run_pass(Pass::Single, opts, 1, &mut sink))
    } else {
        None
    };
    let multi = if opts.run_multi {
        Some(kernel.run_pass(Pass::Multi, opts, opts.threads, &mut sink))
    } else {
        None
    };
    BenchResult { single, multi }
}
