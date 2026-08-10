use std::env;

use cpuz_bench::bench::{run, BenchOptions, BenchResult};
use cpuz_bench::kernel::Engine;

fn usage() -> ! {
    eprintln!(
        "cpuz-bench - CPU-Z benchmark reimplementation in Rust

Usage: cpuz-bench [options]

Options:
  --mode <mode>     Benchmark mode:
                      17.01        (default, SSE)
                      19.01-sse    19.01 SSE
                      19.01-avx2   19.01 AVX2 (requires AVX2 CPU)
                      19.01-avx512 19.01 AVX512 (requires AVX-512 CPU)
  --duration <ms>   Test duration per pass (default 10000)
  --single-only     Run only the single-thread pass
  --multi-only      Run only the multi-thread pass
  --threads <n>     Worker threads for the multi-thread pass (default: logical CPUs)
  -h, --help        Show this help
"
    );
    std::process::exit(1);
}

fn parse_args() -> BenchOptions {
    let mut opts = BenchOptions::default();
    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" => {
                i += 1;
                if i >= args.len() {
                    usage();
                }
                opts.engine = match args[i].as_str() {
                    "17.01" => Engine::V1701,
                    "19.01-sse" | "19.01" => Engine::V1901Sse,
                    "19.01-avx2" => Engine::V1901Avx2,
                    "19.01-avx512" => {
                        eprintln!("note: 19.01-avx512 not implemented yet, using AVX2");
                        Engine::V1901Avx2
                    }
                    other => {
                        eprintln!("unknown mode: {}", other);
                        usage();
                    }
                };
            }
            "--duration" => {
                i += 1;
                if i >= args.len() {
                    usage();
                }
                opts.duration_ms = args[i].parse().unwrap_or_else(|_| usage());
            }
            "--single-only" => {
                opts.run_multi = false;
            }
            "--multi-only" => {
                opts.run_single = false;
            }
            "--threads" => {
                i += 1;
                if i >= args.len() {
                    usage();
                }
                opts.threads = args[i].parse().unwrap_or_else(|_| usage());
            }
            "-h" | "--help" => usage(),
            _ => {
                eprintln!("unknown argument: {}", args[i]);
                usage();
            }
        }
        i += 1;
    }
    opts
}

fn fmt_score(v: f32) -> String {
    format!("{:.0}", v)
}

fn report(name: &str, r: &BenchResult) {
    println!("=== CPU-Z benchmark ({}) ===", name);
    if r.single_ms > 0 {
        println!(
            "CPU (Single Thread) : {:>8}  ({:>8} units in {} ms)",
            fmt_score(r.single_score),
            r.single_units,
            r.single_ms
        );
    }
    if r.multi_ms > 0 {
        println!(
            "CPU (Multi Thread)  : {:>8}  ({:>8} units in {} ms)",
            fmt_score(r.multi_score),
            r.multi_units,
            r.multi_ms
        );
    }
}

fn main() {
    let opts = parse_args();
    let name = opts.engine.name();
    println!(
        "Running {} benchmark: {} ms/pass, {} worker threads",
        name,
        opts.duration_ms,
        opts.threads
    );
    let result = run(&opts);
    report(name, &result);
}
