//! Benchmark engine: worker pool + job dispatch + timing + score + live progress.
//!
//! Faithful port of CPU-Z's benchmark loops:
//! - 17.01 single (sub_14002FC00): one worker, 1024x1024, runs `duration` ms,
//!   score = total_units / ms  (units = rows rendered * width)
//! - 17.01 multi (sub_14002FFC0): all workers, score = total_units / ms
//! - 19.01 single (sub_1400307F0): rows per batch = height/8, score = units/ms/10
//! - 19.01 multi (sub_140030C20): frame dispatcher, score = units/ms/10
//!
//! CPU-Z dispatches work via QueueUserAPC to per-core worker threads; we
//! emulate the same producer/consumer pattern. Workers block on a per-worker
//! `Mutex` + `Condvar` (the portable equivalent of an alertable
//! `WaitForSingleObjectEx`), the dispatcher waits for a free worker without
//! burning an SMT sibling core.
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
//!
//! Live score: `score = completed_units / elapsed_ms_so_far`, which is the
//! same quantity CPU-Z ends up reporting — it converges to the final score
//! as the pass runs, exactly like the UI number slowly settles.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::affinity::pin_thread_to_cpu;
use crate::kernel::{render_rows, Engine};

pub const BENCH_WIDTH: usize = 1024;
pub const BENCH_HEIGHT: usize = 1024;

/// Rows per dispatched job. CPU-Z 17.01 dispatches one row per QueueUserAPC;
/// batching only removes wakeup overhead from the measurement window, it does
/// not change the rendered work or the score. The single-thread pass uses
/// small batches so the live score moves smoothly.
pub const DEFAULT_BATCH_ROWS_SINGLE: usize = 8;
pub const DEFAULT_BATCH_ROWS_MULTI: usize = 16;

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

/// Per-worker hand-off state, protected by the worker's mutex.
struct WorkerState {
    /// Pending job; the worker takes it and clears the slot.
    job: Option<Job>,
    /// True from dispatch until the job has been rendered (dispatcher waits
    /// on this, mirroring CPU-Z's per-row completion check).
    busy: bool,
    stop: bool,
}

struct Worker {
    state: Mutex<WorkerState>,
    cv: Condvar,
    buf: Arc<SharedBuf>,
    /// Completed work units (pixels). Written by the worker on completion,
    /// read by the dispatcher for score bookkeeping and live progress.
    units: AtomicU64,
}

impl Worker {
    fn spawn(
        buf: Arc<SharedBuf>,
        cpu: Option<usize>,
    ) -> (Arc<Self>, std::thread::JoinHandle<()>) {
        let w = Arc::new(Worker {
            state: Mutex::new(WorkerState {
                job: None,
                busy: false,
                stop: false,
            }),
            cv: Condvar::new(),
            buf,
            units: AtomicU64::new(0),
        });
        let wh = Arc::clone(&w);
        let handle = std::thread::spawn(move || {
            if let Some(c) = cpu {
                pin_thread_to_cpu(c);
            }
            worker_loop(wh);
        });
        (w, handle)
    }

    /// Queue a job if the worker is idle. Non-blocking: the multi-thread
    /// dispatcher keeps looking for a free worker.
    fn try_dispatch(&self, job: &Job) -> bool {
        let mut st = match self.state.try_lock() {
            Ok(st) => st,
            Err(_) => return false,
        };
        if st.busy || st.stop {
            return false;
        }
        st.job = Some(*job);
        st.busy = true;
        drop(st);
        self.cv.notify_one();
        true
    }

    /// Queue a job, waiting for the previous one to finish first. Used by the
    /// single-thread pass, which is fully serialized like CPU-Z's 1T driver.
    fn dispatch_wait(&self, job: &Job) {
        let mut st = self.state.lock().unwrap();
        while st.busy {
            st = self.cv.wait(st).unwrap();
        }
        st.job = Some(*job);
        st.busy = true;
        drop(st);
        self.cv.notify_one();
    }

    /// Wait until no job is in flight on this worker.
    fn wait_idle(&self) {
        let mut st = self.state.lock().unwrap();
        while st.busy {
            st = self.cv.wait(st).unwrap();
        }
    }

    fn units(&self) -> u64 {
        self.units.load(Ordering::Acquire)
    }

    /// Tell the worker to exit; returns once it has stopped.
    fn shutdown(&self) {
        let mut st = self.state.lock().unwrap();
        while st.busy {
            st = self.cv.wait(st).unwrap();
        }
        st.stop = true;
        drop(st);
        self.cv.notify_all();
    }
}

fn worker_loop(w: Arc<Worker>) {
    loop {
        let job = {
            let mut st = w.state.lock().unwrap();
            loop {
                if let Some(j) = st.job.take() {
                    break j;
                }
                if st.stop {
                    return;
                }
                st = w.cv.wait(st).unwrap();
            }
        };

        // SAFETY: rows [row0, row0+rows) belong exclusively to this job.
        let buf = unsafe { std::slice::from_raw_parts_mut(w.buf.ptr, w.buf.len) };
        match job.engine {
            #[cfg(target_arch = "x86_64")]
            Engine::V1901Avx2 => unsafe {
                crate::noise_avx2::render_rows_avx2(
                    buf,
                    job.x0,
                    job.y0,
                    job.row0,
                    job.rows,
                    job.width,
                    BENCH_WIDTH * 4,
                );
            },
            // `_` also covers V1901Avx2 on non-x86_64 hosts, where the 19.01
            // SSE kernel is the fallback.
            _ => {
                render_rows(
                    job.engine,
                    buf,
                    job.x0,
                    job.y0,
                    job.row0,
                    job.rows,
                    job.width,
                    BENCH_WIDTH * 4,
                );
            }
        }

        {
            let mut st = w.state.lock().unwrap();
            w.units
                .fetch_add((job.rows * job.width) as u64, Ordering::AcqRel);
            st.busy = false;
        }
        w.cv.notify_all(); // wake a waiting dispatcher
    }
}

/// A pool of pinned workers sharing one frame buffer.
struct Pool {
    workers: Vec<Arc<Worker>>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl Pool {
    /// `cpu(i)` returns the logical CPU for worker `i`, or None to leave it
    /// unpinned. Spawning and pinning happen before timing starts.
    fn new<F: Fn(usize) -> Option<usize>>(n: usize, cpu: F) -> Pool {
        let buf = SharedBuf::new();
        let mut workers = Vec::with_capacity(n);
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let (w, h) = Worker::spawn(Arc::clone(&buf), cpu(i));
            workers.push(w);
            handles.push(h);
        }
        Pool { workers, handles }
    }

    fn total_units(&self) -> u64 {
        self.workers.iter().map(|w| w.units()).sum()
    }

    fn stop_and_join(&mut self) {
        for w in &self.workers {
            w.shutdown();
        }
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.stop_and_join();
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
            report_interval_ms: 200,
            batch_rows_single: DEFAULT_BATCH_ROWS_SINGLE,
            batch_rows_multi: DEFAULT_BATCH_ROWS_MULTI,
        }
    }
}

/// Build a job for the current iteration/row cursor. Advances the cursor.
fn make_job(engine: Engine, iter: &mut i32, row: &mut usize, batch_rows: usize) -> Job {
    let rpj = batch_rows.clamp(1, BENCH_HEIGHT);
    let r = rpj.min(BENCH_HEIGHT - *row);
    let job = Job {
        engine,
        x0: *iter,
        y0: *iter,
        row0: *row as i32,
        rows: r,
        width: BENCH_WIDTH,
    };
    *row += r;
    if *row >= BENCH_HEIGHT {
        *row = 0;
        *iter += 1;
    }
    job
}

/// Run the single-thread benchmark (one worker, fully serialized).
pub fn run_single<F: FnMut(Progress)>(opts: &BenchOptions, on_progress: &mut F) -> PassResult {
    let mut pool = Pool::new(1, |_| Some(0));
    let worker = Arc::clone(&pool.workers[0]);

    let mut row = 0usize;
    let mut iter = 0i32;
    let mut next_report = opts.report_interval_ms;

    let start = Instant::now();
    loop {
        let job = make_job(opts.engine, &mut iter, &mut row, opts.batch_rows_single);
        worker.dispatch_wait(&job);

        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed >= opts.duration_ms {
            break;
        }
        if opts.report_interval_ms > 0 && elapsed >= next_report {
            next_report = elapsed + opts.report_interval_ms;
            on_progress(Progress {
                pass: Pass::Single,
                done: false,
                score: score_of(opts.engine, pool.total_units(), elapsed.max(1)),
                units: pool.total_units(),
                elapsed_ms: elapsed,
                duration_ms: opts.duration_ms,
                threads: 1,
            });
        }
    }
    worker.wait_idle();
    let ms = start.elapsed().as_millis() as u64;
    let units = pool.total_units();
    let score = score_of(opts.engine, units, ms);
    on_progress(Progress {
        pass: Pass::Single,
        done: true,
        score,
        units,
        elapsed_ms: ms,
        duration_ms: opts.duration_ms,
        threads: 1,
    });
    pool.stop_and_join();

    PassResult {
        score,
        units,
        ms,
        threads: 1,
    }
}

/// Run the multi-thread benchmark (round-robin dispatch to all workers, like
/// CPU-Z's frame dispatcher).
pub fn run_multi<F: FnMut(Progress)>(opts: &BenchOptions, on_progress: &mut F) -> PassResult {
    let n = opts.threads.max(1);
    let mut pool = Pool::new(n, Some);

    let mut row = 0usize;
    let mut iter = 0i32;
    let mut next_report = opts.report_interval_ms;

    let start = Instant::now();
    loop {
        let job = make_job(opts.engine, &mut iter, &mut row, opts.batch_rows_multi);

        // Hand the job to a free worker; if all are busy, yield and retry
        // (CPU-Z's dispatcher polls with Sleep(0) as well).
        loop {
            let mut queued = false;
            for w in &pool.workers {
                if w.try_dispatch(&job) {
                    queued = true;
                    break;
                }
            }
            if queued {
                break;
            }
            std::thread::yield_now();
        }

        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed >= opts.duration_ms {
            break;
        }
        if opts.report_interval_ms > 0 && elapsed >= next_report {
            next_report = elapsed + opts.report_interval_ms;
            let units = pool.total_units();
            on_progress(Progress {
                pass: Pass::Multi,
                done: false,
                score: score_of(opts.engine, units, elapsed.max(1)),
                units,
                elapsed_ms: elapsed,
                duration_ms: opts.duration_ms,
                threads: n,
            });
        }
    }

    // Wait for every in-flight job, then stop the pool. The final timestamp
    // is taken after the tail work, exactly like CPU-Z.
    for w in &pool.workers {
        w.wait_idle();
    }
    let ms = start.elapsed().as_millis() as u64;
    let units = pool.total_units();
    let score = score_of(opts.engine, units, ms);
    on_progress(Progress {
        pass: Pass::Multi,
        done: true,
        score,
        units,
        elapsed_ms: ms,
        duration_ms: opts.duration_ms,
        threads: n,
    });
    pool.stop_and_join();

    PassResult {
        score,
        units,
        ms,
        threads: n,
    }
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
