//! Benchmark engine: worker pool + job dispatch + timing + score.
//!
//! Faithful port of CPU-Z's benchmark loops:
//! - 17.01 single (sub_14002FC00): one worker, 1024x1024, runs `duration` ms,
//!   score = total_units / ms  (units = rows rendered * width)
//! - 17.01 multi (sub_14002FFC0): all workers, score = total_units / ms
//! - 19.01 single (sub_1400307F0): rows per batch = height/8, score = units/ms/10
//! - 19.01 multi (sub_140030C20): frame dispatcher, score = units/ms/10
//!
//! CPU-Z dispatches work via QueueUserAPC to per-core worker threads; we
//! emulate the same producer/consumer pattern with busy-flag workers.
//!
//! Job semantics (from worker disassembly):
//!   - 17.01 (sub_14004A9D0): one job = one row. Job fields:
//!       x0 = iter counter, y0 = iter counter, row = v33 (0..height, wraps).
//!     Sample coords: (x0 + i, y0 + row), buffer row = row. Units = width.
//!   - 19.01 (sub_140031130): one job = `rows` rows. Job fields:
//!       x0 = iter, y0 = iter, row0 = row start, rows = batch size.
//!     Sample coords: (x0 + i, y0 + row0 + r), buffer row = row0 + r.
//!     Units = rows * width.
//!
//! The 4 MiB RGBA buffer is shared, exactly like CPU-Z: jobs are dispatched
//! sequentially with monotonically advancing row ranges, so concurrent
//! workers never touch overlapping rows. We use a raw-pointer shared buffer
//! to avoid locking (a Mutex here would serialize workers and destroy the
//! score).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::kernel::{render_rows, Engine};

pub const BENCH_WIDTH: usize = 1024;
pub const BENCH_HEIGHT: usize = 1024;

/// Shared frame buffer with disjoint-row guarantees (see module docs).
struct SharedBuf {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: rows written by workers never overlap (guaranteed by dispatch
// order), and each worker writes distinct memory.
unsafe impl Send for SharedBuf {}
unsafe impl Sync for SharedBuf {}

impl SharedBuf {
    fn new() -> Arc<Self> {
        let mut v = vec![0u8; BENCH_WIDTH * BENCH_HEIGHT * 4];
        let ptr = v.as_mut_ptr();
        let len = v.len();
        std::mem::forget(v); // raw memory, freed on drop below
        Arc::new(SharedBuf { ptr, len })
    }

    /// The 4 MiB buffer pointer (main-thread helper, currently unused).
    #[allow(dead_code)]
    fn as_slice_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for SharedBuf {
    fn drop(&mut self) {
        unsafe {
            drop(Vec::from_raw_parts(self.ptr, self.len, self.len));
        }
    }
}

/// A single render job.
#[derive(Clone, Copy)]
struct Job {
    engine: Engine,
    x0: i32,
    y0: i32,
    row0: i32,
    rows: usize,
    width: usize,
}

/// Worker thread context (mirrors CPU-Z's worker struct fields).
struct Worker {
    busy: AtomicBool,
    units: AtomicU32,
    stop: AtomicBool,
    engine: AtomicU32,
    x0: AtomicI32,
    y0: AtomicI32,
    row0: AtomicI32,
    rows: AtomicU32,
    width: AtomicU32,
    buf: Arc<SharedBuf>,
}

use std::sync::atomic::AtomicI32;

impl Worker {
    fn new(buf: Arc<SharedBuf>) -> Arc<Self> {
        let w = Arc::new(Worker {
            busy: AtomicBool::new(false),
            units: AtomicU32::new(0),
            stop: AtomicBool::new(false),
            engine: AtomicU32::new(0),
            x0: AtomicI32::new(0),
            y0: AtomicI32::new(0),
            row0: AtomicI32::new(0),
            rows: AtomicU32::new(0),
            width: AtomicU32::new(0),
            buf,
        });
        let wh = Arc::clone(&w);
        std::thread::spawn(move || worker_loop(wh));
        w
    }

    /// Queue a job: write fields, publish via busy flag.
    fn dispatch(&self, job: &Job) {
        self.engine.store(job.engine as u32, Ordering::Release);
        self.x0.store(job.x0, Ordering::Release);
        self.y0.store(job.y0, Ordering::Release);
        self.row0.store(job.row0, Ordering::Release);
        self.rows.store(job.rows as u32, Ordering::Release);
        self.width.store(job.width as u32, Ordering::Release);
        self.units.store(0, Ordering::Release);
        self.busy.store(true, Ordering::Release);
    }

    /// Wait until the worker finished its current job.
    fn wait_idle(&self) {
        while self.busy.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
    }
}

fn worker_loop(w: Arc<Worker>) {
    loop {
        while !w.busy.load(Ordering::Acquire) {
            if w.stop.load(Ordering::Acquire) {
                return;
            }
            std::hint::spin_loop();
        }
        let engine = match w.engine.load(Ordering::Acquire) {
            0 => Engine::V1701,
            _ => Engine::V1901,
        };
        let x0 = w.x0.load(Ordering::Acquire);
        let y0 = w.y0.load(Ordering::Acquire);
        let row0 = w.row0.load(Ordering::Acquire);
        let rows = w.rows.load(Ordering::Acquire) as usize;
        let width = w.width.load(Ordering::Acquire) as usize;

        // SAFETY: rows [row0, row0+rows) belong exclusively to this job.
        let buf = unsafe { std::slice::from_raw_parts_mut(w.buf.ptr, w.buf.len) };
        render_rows(engine, buf, x0, y0, row0, rows, width, BENCH_WIDTH * 4);
        w.units.store((rows * width) as u32, Ordering::Release);
        w.busy.store(false, Ordering::Release);
    }
}

pub struct BenchResult {
    pub single_score: f32,
    pub multi_score: f32,
    pub single_units: u64,
    pub multi_units: u64,
    pub single_ms: u64,
    pub multi_ms: u64,
}

pub struct BenchOptions {
    pub engine: Engine,
    pub duration_ms: u64,
    pub run_single: bool,
    pub run_multi: bool,
    /// Logical CPU count to use for the multi-threaded pass.
    pub threads: usize,
}

impl Default for BenchOptions {
    fn default() -> Self {
        BenchOptions {
            engine: Engine::V1701,
            duration_ms: 10000,
            run_single: true,
            run_multi: true,
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        }
    }
}

/// Build a job for the current iteration/row cursor. Advances the cursor.
fn make_job(engine: Engine, iter: &mut i32, row: &mut usize) -> Job {
    let w = BENCH_WIDTH as i32;
    match engine {
        Engine::V1701 => {
            // row = v33 wraps at height; iter = v26 increments each wrap
            let job = Job {
                engine,
                x0: *iter,
                y0: *iter,
                row0: *row as i32,
                rows: 1,
                width: w as usize,
            };
            *row += 1;
            if *row >= BENCH_HEIGHT {
                *row = 0;
                *iter += 1;
            }
            job
        }
        Engine::V1901 => {
            // row0 = v14 advances by height/8, wraps at height; iter = v15
            let rpj = BENCH_HEIGHT / 8;
            let r = rpj.min(BENCH_HEIGHT - *row);
            let job = Job {
                engine,
                x0: *iter,
                y0: *iter,
                row0: *row as i32,
                rows: r,
                width: w as usize,
            };
            *row += r;
            if *row >= BENCH_HEIGHT {
                *row = 0;
                *iter += 1;
            }
            job
        }
    }
}

/// Run the single-thread benchmark (one worker).
/// Returns (score, units, ms).
pub fn run_single(opts: &BenchOptions) -> (f32, u64, u64) {
    let buf = SharedBuf::new();
    let worker = Worker::new(Arc::clone(&buf));

    let mut units: u64 = 0;
    let mut row = 0usize;
    let mut iter = 0i32;

    let start = Instant::now();
    loop {
        worker.wait_idle();
        units += worker.units.load(Ordering::Acquire) as u64;

        let job = make_job(opts.engine, &mut iter, &mut row);
        worker.dispatch(&job);

        if start.elapsed().as_millis() as u64 >= opts.duration_ms {
            break;
        }
    }
    worker.wait_idle();
    units += worker.units.load(Ordering::Acquire) as u64;
    worker.stop.store(true, Ordering::Release);
    worker.busy.store(true, Ordering::Release); // wake to exit

    let ms = start.elapsed().as_millis() as u64;
    let score = score_of(opts.engine, units, ms);
    (score, units, ms)
}

/// Run the multi-thread benchmark (all workers, round-robin dispatch like
/// CPU-Z's frame dispatcher). Returns (score, units, ms).
pub fn run_multi(opts: &BenchOptions) -> (f32, u64, u64) {
    let n = opts.threads.max(1);
    let buf = SharedBuf::new();
    let workers: Vec<Arc<Worker>> = (0..n).map(|_| Worker::new(Arc::clone(&buf))).collect();

    let mut units: u64 = 0;
    let mut row = 0usize;
    let mut iter = 0i32;

    let start = Instant::now();
    loop {
        // find an idle worker, wait for its completion
        let mut idx = None;
        loop {
            for (i, wk) in workers.iter().enumerate() {
                if !wk.busy.load(Ordering::Acquire) {
                    idx = Some(i);
                    break;
                }
            }
            match idx {
                Some(_) => break,
                None => std::hint::spin_loop(),
            }
        }
        let wk = &workers[idx.unwrap()];
        units += wk.units.load(Ordering::Acquire) as u64;

        let job = make_job(opts.engine, &mut iter, &mut row);
        wk.dispatch(&job);

        if start.elapsed().as_millis() as u64 >= opts.duration_ms {
            break;
        }
    }
    // wait for all in-flight jobs and collect
    for wk in workers.iter() {
        wk.wait_idle();
        units += wk.units.load(Ordering::Acquire) as u64;
        wk.stop.store(true, Ordering::Release);
        wk.busy.store(true, Ordering::Release);
    }

    let ms = start.elapsed().as_millis() as u64;
    let score = score_of(opts.engine, units, ms);
    (score, units, ms)
}

/// Score formula: 17.01 = units/ms (sub_14002FC00 returns (float)v13 / v31);
/// 19.01 = units/ms/10 (sub_1400307F0 returns v38/v36/10, sub_140030C20
/// returns freq*units/elapsed/1000/10).
fn score_of(engine: Engine, units: u64, ms: u64) -> f32 {
    if ms == 0 {
        return 0.0;
    }
    let base = units as f32 / ms as f32;
    match engine {
        Engine::V1701 => base,
        Engine::V1901 => base / 10.0,
    }
}

/// Full benchmark: single-thread pass then multi-thread pass.
pub fn run(opts: &BenchOptions) -> BenchResult {
    let (single_score, single_units, single_ms) = if opts.run_single {
        run_single(opts)
    } else {
        (0.0, 0, 0)
    };
    let (multi_score, multi_units, multi_ms) = if opts.run_multi {
        run_multi(opts)
    } else {
        (0.0, 0, 0)
    };
    BenchResult {
        single_score,
        multi_score,
        single_units,
        multi_units,
        single_ms,
        multi_ms,
    }
}
