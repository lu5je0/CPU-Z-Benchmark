use std::time::Instant;
use cpuz_bench::kernel::{pixel, Engine};

fn bench(name: &str, engine: Engine) {
    let iters = 200_000;
    let start = Instant::now();
    let mut acc = 0u64;
    for i in 0..iters {
        let p = pixel(engine, (i % 1024) as i32, (i * 7 % 1024) as i32);
        acc += p[0] as u64 + p[1] as u64;
    }
    let el = start.elapsed();
    println!(
        "{:<16} {:>8} px in {:>8.1} ms  ({:>6.1} ns/px) acc={}",
        name,
        iters,
        el.as_secs_f64() * 1000.0,
        el.as_secs_f64() * 1e9 / iters as f64,
        acc
    );
}

fn main() {
    bench("17.01 SSE", Engine::V1701);
    bench("19.01 SSE", Engine::V1901Sse);
    bench("19.01 AVX2", Engine::V1901Avx2);
}
