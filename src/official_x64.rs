//! Run CPU-Z's own Windows-x64 benchmark kernel natively on this machine.
//!
//! This is the x86_64 counterpart of the aarch64 loader in `official.rs`.
//! CPUID ships `cpuz_x64.exe`; the 17.01 render kernel inside it is a
//! self-contained tree of functions (row worker -> pixel loop -> noise A/B ->
//! simplex sample -> gradient lookup) that only reads a small parameter block,
//! one 4096-float gradient table handed in through the context pointer, and
//! RIP-relative float constants from `.rdata`. Nothing in that call tree
//! touches an import, so we can map the user's own copy of the executable and
//! call the kernel directly -- no Windows PE loader and no emulation.
//!
//! Nothing here bundles or redistributes the executable: the user supplies it
//! via `--official <path>`, `CPUZ_X64_EXE`, or the shared cache path that the
//! `q-cpuz-benchmark` wrapper fills in.
//!
//! Layout recovered from the CPU-Z 3.01 disassembly (benchmark version 17.01):
//!
//! * the row worker is a 65-byte straight-line function whose parameter-block
//!   prologue (`mov eax,[rdx+0x34]`, `mov r9d,[rdx+0x2c]`, `mov r8d,[rdx+0x28]`)
//!   is unique in `.text` for CPU-Z 2.10 through 3.01, which is how we locate
//!   it without hard-coding an address. It reads
//!   `+0x18 ctx, +0x20 dst, +0x28 x0, +0x2c y0, +0x30 width, +0x34 row`, writes
//!   the row's pixel count to `+0x3c work` and clears `+0x40 active`, exactly
//!   like the ARM64 build and like this crate's own worker pool;
//! * the context only needs the gradient table pointer at `+0x30` (the ARM64
//!   build uses `+0x38`), everything else is read by the object initialiser we
//!   deliberately do not run;
//! * one call renders one 1024-pixel row and reports 1024 work units, so a
//!   1024x1024 frame reports 1048576 units and the score unit is pixels/ms.
//!
//! Calling convention
//! ==================
//! The kernel is compiled for the Microsoft x64 ABI (args in rcx/rdx/r8/r9,
//! rdi/rsi nonvolatile). We enter at the row worker, which only reads its
//! parameter block from `rdx` and uses no other argument register, so a
//! three-argument `extern "C"` call puts the block in `rdx` (the third SysV
//! argument register) and is enough. The callee preserves every nonvolatile
//! register the SysV ABI expects, and it never touches the shadow space.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use crate::bench::{BenchOptions, BenchResult, Pass, PassResult, Progress};

pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 1024;

/// Offset of the gradient-table pointer inside the benchmark context.
const CTX_TABLE_OFFSET: usize = 0x30;

#[repr(C)]
struct Ctx {
    pad: [u8; CTX_TABLE_OFFSET],
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

/// The official row worker. `param` must land in `rdx`, hence the two unused
/// leading arguments (see the module docs).
type RowFn = unsafe extern "C" fn(*mut u8, *mut u8, *mut Param);

/// A mapped, executable copy of the official kernel.
pub struct Kernel {
    code: *mut u8,
    len: usize,
    row: RowFn,
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

/// A PE section reduced to what the loader needs.
struct Section {
    name: [u8; 8],
    va: u32,
    vsize: u32,
    file_off: u32,
    raw_size: u32,
}

/// The bits of the PE we need to place the image and find the kernel.
struct Pe<'a> {
    bytes: &'a [u8],
    size_of_image: usize,
    size_of_headers: usize,
    sections: Vec<Section>,
    /// `.pdata` function ranges as `(begin_va, end_va)`, merged so a
    /// shrink-wrapped prologue and its body count as one function.
    functions: Vec<(u32, u32)>,
}

fn rd_u16(b: &[u8], off: usize) -> Result<u16, String> {
    let s = b.get(off..off + 2).ok_or("truncated PE header")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn rd_u32(b: &[u8], off: usize) -> Result<u32, String> {
    let s = b.get(off..off + 4).ok_or("truncated PE header")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd_u64(b: &[u8], off: usize) -> Result<u64, String> {
    let s = b.get(off..off + 8).ok_or("truncated PE header")?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

impl<'a> Pe<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Pe<'a>, String> {
        let e_lfanew = rd_u32(bytes, 0x3C)? as usize;
        if bytes.get(e_lfanew..e_lfanew + 4) != Some(b"PE\0\0") {
            return Err("not a PE image".to_string());
        }
        let coff = e_lfanew + 4;
        let nsec = rd_u16(bytes, coff + 2)? as usize;
        let opt_size = rd_u16(bytes, coff + 16)? as usize;
        let opt = coff + 20;
        let magic = rd_u16(bytes, opt)?;
        let (_, dd_off) = match magic {
            0x20b => (rd_u64(bytes, opt + 0x18)?, opt + 0x70),
            0x10b => (rd_u32(bytes, opt + 0x1C)? as u64, opt + 0x60),
            _ => return Err("not a PE32/PE32+ image".to_string()),
        };
        let size_of_image = rd_u32(bytes, opt + 0x38)? as usize;
        let size_of_headers = rd_u32(bytes, opt + 0x3C)? as usize;

        let sec_off = opt + opt_size;
        let mut sections = Vec::with_capacity(nsec);
        for i in 0..nsec {
            let o = sec_off + i * 40;
            let mut name = [0u8; 8];
            name.copy_from_slice(bytes.get(o..o + 8).ok_or("truncated section table")?);
            sections.push(Section {
                name,
                vsize: rd_u32(bytes, o + 8)?,
                va: rd_u32(bytes, o + 12)?,
                raw_size: rd_u32(bytes, o + 16)?,
                file_off: rd_u32(bytes, o + 20)?,
            });
        }

        // Exception directory (index 3) gives every function's extent.
        let exc_rva = rd_u32(bytes, dd_off + 3 * 8)? as usize;
        let exc_size = rd_u32(bytes, dd_off + 3 * 8 + 4)? as usize;
        let mut functions = Vec::new();
        if exc_rva != 0 && exc_size >= 12 {
            let exc_off = rva_to_file(&sections, exc_rva)
                .ok_or("exception directory outside the image")?;
            for i in 0..exc_size / 12 {
                let o = exc_off + i * 12;
                let beg = rd_u32(bytes, o)?;
                let end = rd_u32(bytes, o + 4)?;
                if end > beg {
                    functions.push((beg, end));
                }
            }
        }
        functions.sort_unstable();
        // Merge chained ranges (shrink-wrapped prologues) and duplicates.
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(functions.len());
        for (beg, end) in functions {
            match merged.last_mut() {
                Some(last) if beg <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((beg, end)),
            }
        }

        Ok(Pe {
            bytes,
            size_of_image,
            size_of_headers,
            sections,
            functions: merged,
        })
    }

    fn section(&self, name: &str) -> Option<&Section> {
        let want = name.as_bytes();
        self.sections.iter().find(|s| {
            let end = s.name.iter().position(|&c| c == 0).unwrap_or(s.name.len());
            &s.name[..end] == want
        })
    }

    fn function_of(&self, va: u32) -> Option<(u32, u32)> {
        let idx = self.functions.partition_point(|&(beg, _)| beg <= va);
        if idx == 0 {
            return None;
        }
        let f = self.functions[idx - 1];
        (va < f.1).then_some(f)
    }
}

fn rva_to_file(sections: &[Section], rva: usize) -> Option<usize> {
    for s in sections {
        let span = s.raw_size.max(s.vsize) as usize;
        if rva >= s.va as usize && rva < s.va as usize + span {
            return Some(s.file_off as usize + (rva - s.va as usize));
        }
    }
    None
}

/// Find the official 17.01 row worker in a parsed image.
///
/// The signature is the worker's parameter-block prologue: `mov eax,[rdx+0x34]`,
/// `mov r9d,[rdx+0x2c]`, `mov r8d,[rdx+0x28]`. It is unique in `.text` for every
/// CPU-Z build we checked (2.10, 2.15, 2.18, 3.01), where the worker is the same
/// 65-byte straight-line function, so no address is hard-coded.
const ROW_WORKER_SIG: [u8; 11] = [
    0x8b, 0x42, 0x34, // mov  eax, [rdx+0x34]
    0x44, 0x8b, 0x4a, 0x2c, // mov  r9d, [rdx+0x2c]
    0x44, 0x8b, 0x42, 0x28, // mov  r8d, [rdx+0x28]
];

fn locate_row_worker(pe: &Pe) -> Result<u32, String> {
    let text = pe
        .section(".text")
        .ok_or("no .text section: not cpuz_x64.exe?")?;
    let text_off = text.file_off as usize;
    let text_len = text.raw_size as usize;
    let text_bytes = pe
        .bytes
        .get(text_off..text_off + text_len)
        .ok_or("truncated .text section")?;

    let mut found: Vec<(u32, u32)> = Vec::new();
    let mut pos = 0;
    while let Some(off) = find(&text_bytes[pos..], &ROW_WORKER_SIG) {
        let va = text.va + (pos + off) as u32;
        if let Some(f) = pe.function_of(va) {
            if !found.contains(&f) {
                found.push(f);
            }
        }
        pos += off + 1;
    }
    match found.as_slice() {
        [row] => Ok(row.0),
        [] => Err("17.01 row worker not found (unsupported CPU-Z build?)".to_string()),
        many => Err(format!(
            "ambiguous 17.01 row worker: {} candidates ({:x?})",
            many.len(),
            many.iter().map(|f| f.0).collect::<Vec<_>>()
        )),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

impl Kernel {
    pub fn load(path: &Path) -> Result<Kernel, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if bytes.len() < 0x1000 {
            return Err(format!("{}: too small to be cpuz_x64.exe", path.display()));
        }
        let pe = Pe::parse(&bytes)?;
        let row_va = locate_row_worker(&pe)?;

        // Map the whole image with every section at its virtual address, so
        // RIP-relative loads of the .rdata constants stay valid. These builds
        // have no relocation directory, but the kernel path only ever uses
        // PC-relative addressing, so the mapping base does not matter.
        let len = pe.size_of_image;
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
            let base = code as *mut u8;
            // Headers and section contents, each at its image offset.
            let headers = pe.size_of_headers.min(len);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), base, headers);
            for s in &pe.sections {
                let start = s.file_off as usize;
                let n = (s.vsize.min(s.raw_size)) as usize;
                if start + n > bytes.len() || s.va as usize + n > len {
                    continue;
                }
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(start),
                    base.add(s.va as usize),
                    n,
                );
            }
            if libc::mprotect(code, len, libc::PROT_READ | libc::PROT_EXEC) != 0 {
                libc::munmap(code, len);
                return Err("mprotect(PROT_EXEC) failed".to_string());
            }
        }
        let row: RowFn =
            unsafe { std::mem::transmute((code as *mut u8).add(row_va as usize)) };

        Ok(Kernel {
            code: code as *mut u8,
            len,
            row,
            path: path.to_path_buf(),
            version: build_label(bytes.len()),
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
            pad: [0; CTX_TABLE_OFFSET],
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
                let (next, units, stop, ctx, row) = (&next, &units, &stop, &ctx, self.row);
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
                        let row_idx = (idx % height) as i32;
                        param.x0 = frame;
                        param.y0 = frame;
                        param.row = row_idx;
                        param.work = 0;
                        unsafe { row(std::ptr::null_mut(), std::ptr::null_mut(), &mut param) };
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

/// Identify the build from its size, like the ARM64 loader does. The exe
/// version is in a UTF-16 resource, so the size is the simplest stable marker.
fn build_label(len: usize) -> String {
    match len {
        4_912_992 => "2.10".to_string(),
        5_065_944 => "2.15".to_string(),
        7_362_792 => "2.18".to_string(),
        7_447_032 => "3.01".to_string(),
        n => format!("(unrecognised build, {n} bytes)"),
    }
}

/// Cache location shared with the `q-cpuz-benchmark` wrapper.
///
/// `CPUZ_BENCHMARK_CACHE_DIR` is the directory that holds the binaries (the
/// wrapper's `CPUZ_BENCH_CACHE_DIR`), while the other two are base directories
/// that get a `cpuz-benchmark` component appended.
pub fn default_cache_path() -> PathBuf {
    let dir = match std::env::var_os("CPUZ_BENCHMARK_CACHE_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => {
            let base = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
                });
            base.join("cpuz-benchmark")
        }
    };
    dir.join("cpuz_x64.exe")
}

/// Where the official executable is, if we can find it.
pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("CPUZ_X64_EXE") {
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
