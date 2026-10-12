//! Benchmark engine: pinned worker pool, self-driven work pulling, timing and
//! live progress.
//!
//! Faithful port of CPU-Z's benchmark loops:
//! - 17.01 single (sub_14002FC00): one worker, 1024x1024, runs `duration` ms,
//!   score = total_units / ms  (units = rows rendered * width)
//! - 17.01 multi (sub_14002FFC0): all workers, score = total_units / ms
//! - 19.01 single (sub_1400307F0): rows per batch = height/8, score = units/ms/10
//! - 19.01 multi (sub_140030C20): frame dispatcher, score = units/ms/10
//!
//! Work distribution
//! =================
//! CPU-Z dispatches rows through QueueUserAPC: the dispatcher thread never
//! blocks on the renderer, so the worker always has the next row queued and
//! never idles between rows. We reproduce that property with a shared,
//! monotonically increasing row cursor (`next`) that workers advance with a
//! single atomic RMW — no per-job hand-shake at all.
//!
//! This matters for the score: a dispatcher-side hand-shake (even a condition
//! variable hand-off) can stall the single worker for a scheduler tick, which
//! measured as a halved single-thread score in one run. Pulling work from an
//! atomic cursor keeps every worker busy for the whole window.
//!
//! Concurrency safety of the shared frame buffer
//! ============================================
//! All workers write into the same 4 MiB RGBA buffer, exactly like CPU-Z.
//! The cursor only ever advances, and the multi-threaded batch size is clamped
//! so that `threads * batch <= BENCH_HEIGHT`, which guarantees that the rows
//! in flight at any moment are pairwise disjoint (rows of different frames
//! would otherwise alias the same memory).
//!
//! Live score: `score = completed_units / elapsed_ms_so_far`, the same quantity
//! CPU-Z reports at the end — it converges while the pass runs.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use crate::affinity::pin_thread_to_cpu;
use crate::kernel::{render_rows, Engine};

pub const BENCH_WIDTH: usize = 1024;
pub const BENCH_HEIGHT: usize = 1024;

/// CPU-Z dispatches one row per APC in 17.01; keeping a single row per job for
/// the 1T pass reproduces that exactly (and makes the live score very smooth).
pub const DEFAULT_BATCH_ROWS_SINGLE: usize = 1;
/// Rows per job for the multi-threaded pass. Batching only amortises the
/// dispatcher cost; it does not change the rendered work.
pub const DEFAULT_BATCH_ROWS_MULTI: usize = 8;

/// Shared frame buffer. Workers never touch the same row concurrently, see the
/// module docs for the disjointness argument.
struct SharedBuf {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: rows written by workers are pairwise disjoint (see module docs) and
// every worker writes a distinct memory range of the same allocation.
unsafe impl Send for SharedBuf {}
unsafe impl Sync for SharedBuf {}

impl SharedBuf {
    fn new() -> Arc<Self> {
        let mut v = vec![0u8; BENCH_WIDTH * BENCH_HEIGHT * 4];
        let ptr = v.as_mut_ptr();
        let len = v.len();
        std::mem::forget(v); // handed to the raw SharedBuf, freed in Drop
        Arc::new(SharedBuf { ptr, len })
    }
}

impl Drop for SharedBuf {
    fn drop(&mut self) {
        unsafe {
            drop(Vec::from_raw_parts(self.ptr, self.len, self.len));
        }
    }
}

/// State shared by every worker of one pass.
struct PassState {
    buf: Arc<SharedBuf>,
    engine: Engine,
    batch: usize,
    /// Row cursor: multiples of `batch`, monotonically increasing.
    next: AtomicU64,
    /// Completed work units (pixels) of this pass.
    units: AtomicU64,
    stop: AtomicBool,
}

impl PassState {
    fn new(buf: Arc<SharedBuf>, engine: Engine, batch: usize) -> Arc<Self> {
        Arc::new(PassState {
            buf,
            engine,
            batch: batch.clamp(1, BENCH_HEIGHT),
            next: AtomicU64::new(0),
            units: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        })
    }

    /// Take the next job, or None once the pass has been stopped.
    fn take_job(&self) -> Option<(u64, usize)> {
        if self.stop.load(Ordering::Relaxed) {
            return None;
        }
        let idx = self.next.fetch_add(self.batch as u64, Ordering::Relaxed);
        Some((idx, self.batch))
    }
}

fn worker_loop(pass: Arc<PassState>, barrier: Arc<Barrier>, cpu: Option<usize>) {
    if let Some(c) = cpu {
        pin_thread_to_cpu(c);
    }
    barrier.wait(); // released together with the timing thread

    while let Some((idx, batch)) = pass.take_job() {
        let iter = (idx / BENCH_HEIGHT as u64) as i32;
        let row0 = (idx % BENCH_HEIGHT as u64) as usize;
        let rows = batch.min(BENCH_HEIGHT - row0);

        // SAFETY: the rows [row0, row0 + rows) of this job are exclusively
        // owned by it, the borrow ends before the job is accounted.
        let buf = unsafe { std::slice::from_raw_parts_mut(pass.buf.ptr, pass.buf.len) };
        match pass.engine {
            #[cfg(target_arch = "x86_64")]
            Engine::V1901Avx2 => unsafe {
                crate::noise_avx2::render_rows_avx2(
                    buf,
                    iter,
                    iter,
                    row0 as i32,
                    rows,
                    BENCH_WIDTH,
                    BENCH_WIDTH * 4,
                );
            },
            // `_` also covers V1901Avx2 on non-x86_64 hosts, where the 19.01
            // SSE kernel is the fallback.
            _ => {
                render_rows(
                    pass.engine,
                    buf,
                    iter,
                    iter,
                    row0 as i32,
                    rows,
                    BENCH_WIDTH,
                    BENCH_WIDTH * 4,
                );
            }
        }

        pass.units
            .fetch_add((rows * BENCH_WIDTH) as u64, Ordering::Relaxed);
    }
}

/// A worker pool for one pass: pinned threads pulling rows from `PassState`.
struct Pool {
    pass: Arc<PassState>,
    barrier: Arc<Barrier>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

/// Rolling window of `(elapsed_ms, units)` samples used for the live score.
///
/// The printed value is the instantaneous rate over roughly the last
/// [`LiveWindow::window_ms`] — not the average since the pass started. That is
/// what makes the number appear at its steady value right away instead of
/// creeping up from the CPU's clock-ramp at the beginning of the pass. The
/// final number reported for the pass stays the plain `units / ms` average,
/// exactly like CPU-Z.
struct LiveWindow {
    samples: VecDeque<(u64, u64)>,
    window_ms: u64,
}

impl LiveWindow {
    fn new(duration_ms: u64) -> Self {
        LiveWindow {
            // 1/4 of the pass, bounded so short passes still settle quickly.
            samples: VecDeque::from([(0u64, 0u64)]),
            window_ms: (duration_ms / 4).clamp(250, 1000),
        }
    }

    fn push(&mut self, elapsed_ms: u64, units: u64) {
        self.samples.push_back((elapsed_ms, units));
        let target = elapsed_ms.saturating_sub(self.window_ms);
        // Keep the newest sample that is still at or before the window edge as
        // the left bound, so the window is at least `window_ms` wide.
        while self.samples.len() > 2 && self.samples[1].0 <= target {
            self.samples.pop_front();
        }
    }

    /// Instantaneous rate over the retained window.
    fn rate(&self, engine: Engine, elapsed_ms: u64, units: u64) -> f32 {
        let (t0, u0) = *self.samples.front().unwrap_or(&(0, 0));
        let dt = elapsed_ms.saturating_sub(t0).max(1);
        score_of(engine, units.saturating_sub(u0), dt)
    }
}

impl Pool {
    /// `cpu(i)` returns the logical CPU for worker `i`, or None to leave it
    /// unpinned. Threads are spawned and pinned before the timing window opens.
    fn new<F>(n: usize, cpu: F, buf: Arc<SharedBuf>, engine: Engine, batch: usize) -> Pool
    where
        F: Fn(usize) -> Option<usize>,
    {
        let pass = PassState::new(buf, engine, batch);
        let barrier = Arc::new(Barrier::new(n + 1));
        let handles = (0..n)
            .map(|i| {
                let pass = Arc::clone(&pass);
                let barrier = Arc::clone(&barrier);
                let cpu = cpu(i);
                std::thread::spawn(move || worker_loop(pass, barrier, cpu))
            })
            .collect();
        Pool {
            pass,
            barrier,
            handles,
        }
    }

    /// Release every worker and start the measurement window.
    fn run<F>(&mut self, opts: &BenchOptions, pass_kind: Pass, on_progress: &mut F) -> PassResult
    where
        F: FnMut(Progress),
    {
        let threads = self.handles.len();
        let start = Instant::now();
        self.barrier.wait();

        let mut window = LiveWindow::new(opts.duration_ms);
        let mut next_report = opts.report_interval_ms;
        loop {
            let elapsed = start.elapsed().as_millis() as u64;
            if elapsed >= opts.duration_ms {
                break;
            }
            if opts.report_interval_ms > 0 && elapsed >= next_report {
                next_report = elapsed + opts.report_interval_ms;
                let units = self.pass.units.load(Ordering::Relaxed);
                window.push(elapsed, units);
                on_progress(Progress {
                    pass: pass_kind,
                    done: false,
                    score: window.rate(opts.engine, elapsed, units),
                    units,
                    elapsed_ms: elapsed,
                    duration_ms: opts.duration_ms,
                    threads,
                });
                continue; // re-check the deadline before sleeping again
            }
            let remaining = opts.duration_ms.saturating_sub(elapsed);
            std::thread::sleep(Duration::from_millis(remaining.min(2).max(1)));
        }

        self.pass.stop.store(true, Ordering::Release);
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
        // Includes the tail (at most one job per worker, i.e. <= batch rows),
        // so the measured window is never shorter than the work it accounts.
        let ms = start.elapsed().as_millis() as u64;
        let units = self.pass.units.load(Ordering::Relaxed);
        let score = score_of(opts.engine, units, ms);
        on_progress(Progress {
            pass: pass_kind,
            done: true,
            score,
            units,
            elapsed_ms: ms,
            duration_ms: opts.duration_ms,
            threads,
        });

        PassResult {
            score,
            units,
            ms,
            threads,
        }
    }
}

pub struct PassResult {
    pub score: f32,
    pub units: u64,
    pub ms: u64,
    pub threads: usize,
}

pub struct BenchResult {
    pub single: Option<PassResult>,
    pub multi: Option<PassResult>,
}

impl BenchResult {
    pub fn single_score(&self) -> f32 {
        self.single.as_ref().map(|r| r.score).unwrap_or(0.0)
    }
    pub fn multi_score(&self) -> f32 {
        self.multi.as_ref().map(|r| r.score).unwrap_or(0.0)
    }

    /// Multi/single scaling factor, when both passes ran and the single-thread
    /// score is non-zero.
    pub fn ratio(&self) -> Option<f32> {
        let single = self.single.as_ref()?.score;
        if single <= 0.0 {
            return None;
        }
        Some(self.multi.as_ref()?.score / single)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pass {
    Single,
    Multi,
}

/// One live-progress sample. `done` marks the final sample of a pass.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub pass: Pass,
    pub done: bool,
    pub score: f32,
    pub units: u64,
    pub elapsed_ms: u64,
    pub duration_ms: u64,
    pub threads: usize,
}

pub struct BenchOptions {
    pub engine: Engine,
    pub duration_ms: u64,
    pub run_single: bool,
    pub run_multi: bool,
    /// Logical CPU count to use for the multi-threaded pass.
    pub threads: usize,
    /// Live progress sampling interval; 0 disables progress callbacks.
    pub report_interval_ms: u64,
    /// Rows per job for the single-thread pass.
    pub batch_rows_single: usize,
    /// Rows per job for the multi-thread pass.
    pub batch_rows_multi: usize,
}

impl Default for BenchOptions {
    fn default() -> Self {
        BenchOptions {
            engine: Engine::V1701,
            duration_ms: 10000,
            run_single: true,
            run_multi: true,
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            report_interval_ms: 100,
            batch_rows_single: DEFAULT_BATCH_ROWS_SINGLE,
            batch_rows_multi: DEFAULT_BATCH_ROWS_MULTI,
        }
    }
}

/// Which CPU the single-thread pass pins its only worker to.
///
/// CPU-Z uses logical CPU 0. That is right on a desktop, where the cores are
/// equivalent, but on a big.LITTLE mobile SoC the kernel numbers the fastest
/// cluster last, so CPU 0 is a mid-tier performance core and the prime cores
/// stay idle. Android therefore pins to the fastest reachable core; every other
/// platform keeps CPU-Z's CPU 0, which is what the validated x86 and Apple
/// Silicon numbers were measured with. See `affinity::fastest_cpu`.
#[cfg(target_os = "android")]
fn single_thread_cpu() -> Option<usize> {
    crate::affinity::fastest_cpu().or(Some(0))
}

#[cfg(not(target_os = "android"))]
fn single_thread_cpu() -> Option<usize> {
    Some(0)
}

/// Run the single-thread benchmark: one worker, all logical CPUs are free for
/// it, and the work is the same row sequence CPU-Z's 1T pass renders.
pub fn run_single<F: FnMut(Progress)>(opts: &BenchOptions, on_progress: &mut F) -> PassResult {
    let buf = SharedBuf::new();
    let mut pool = Pool::new(
        1,
        |_| single_thread_cpu(),
        buf,
        opts.engine,
        opts.batch_rows_single,
    );
    pool.run(opts, Pass::Single, on_progress)
}

/// Run the multi-thread benchmark: one pinned worker per logical CPU.
pub fn run_multi<F: FnMut(Progress)>(opts: &BenchOptions, on_progress: &mut F) -> PassResult {
    let threads = opts.threads.max(1);
    // Keep the in-flight rows pairwise disjoint: rows of different frames
    // share the same buffer memory, so `threads * batch` must stay inside one
    // frame (see module docs).
    let batch = opts
        .batch_rows_multi
        .clamp(1, BENCH_HEIGHT)
        .min(BENCH_HEIGHT / threads.max(1) + 1)
        .max(1);

    let buf = SharedBuf::new();
    let mut pool = Pool::new(threads, Some, buf, opts.engine, batch);
    pool.run(opts, Pass::Multi, on_progress)
}

/// Score formula: 17.01 = units/ms (sub_14002FC00 returns (float)v13 / v31);
/// 19.01 = units/ms/10 (sub_1400307F0 returns v38/v36/10, sub_140030C20
/// returns freq*units/elapsed/1000/10).
fn score_of(engine: Engine, units: u64, ms: u64) -> f32 {
    if ms == 0 {
        return 0.0;
    }
    let base = units as f32 / ms as f32;
    if engine.div10() {
        base / 10.0
    } else {
        base
    }
}

/// Full benchmark: single-thread pass then multi-thread pass.
pub fn run<F: FnMut(Progress)>(opts: &BenchOptions, on_progress: &mut F) -> BenchResult {
    let single = if opts.run_single {
        Some(run_single(opts, on_progress))
    } else {
        None
    };
    let multi = if opts.run_multi {
        Some(run_multi(opts, on_progress))
    } else {
        None
    };
    BenchResult { single, multi }
}
