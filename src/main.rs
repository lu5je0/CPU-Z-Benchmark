//! CLI front-end with a CPU-Z style live score panel.
//!
//! While a pass runs the score is re-computed as `completed_units / elapsed_ms`
//! every `--interval` milliseconds, so it appears immediately and settles
//! towards the final number instead of only showing up at the end.

use std::env;
use std::io::{IsTerminal, Write};

use cpuz_bench::bench::{run, BenchOptions, BenchResult, Pass, Progress};
use cpuz_bench::kernel::Engine;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BAR_WIDTH: usize = 24;

struct Cli {
    bench: BenchOptions,
    json: bool,
    quiet: bool,
    live: bool,
    cpu: String,
}

fn usage(out: &mut dyn Write) {
    let _ = write!(
        out,
        "cpuz-bench {VERSION} - CPU-Z benchmark reimplementation

Usage: cpuz-bench [options]

Options:
  --mode <mode>      Benchmark mode:
                       17.01        (default, SSE)
                       19.01-sse    19.01 SSE
                       19.01-avx2   19.01 AVX2 (requires AVX2 CPU)
                       19.01-avx512 19.01 AVX512 (falls back to AVX2)
  --duration <ms>    Test duration per pass (default 10000)
  --interval <ms>    Live score sampling interval (default 200, 0 = off)
  --batch-rows <n>   Rows per dispatched job (default: single 8, multi 16)
  --threads <n>      Worker threads for the multi-thread pass (default: all CPUs)
  --single-only      Run only the single-thread pass
  --multi-only       Run only the multi-thread pass
  --no-live          Disable the live score panel
  --json             Machine readable output
  -q, --quiet        Only print the score lines
  -V, --version      Print version
  -h, --help         Show this help

Live score: score = rendered pixels / elapsed ms, the same quantity CPU-Z
reports. It is printed while the pass runs and converges to the final value.
"
    );
}

fn parse_args() -> Result<Cli, ()> {
    let mut opts = BenchOptions::default();
    let mut json = false;
    let mut quiet = false;
    let mut live = std::io::stdout().is_terminal();
    let mut batch_single: Option<usize> = None;
    let mut batch_multi: Option<usize> = None;

    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" => {
                i += 1;
                if i >= args.len() {
                    return Err(());
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
                        eprintln!("unknown mode: {other}");
                        return Err(());
                    }
                };
            }
            "--duration" => {
                i += 1;
                if i >= args.len() {
                    return Err(());
                }
                opts.duration_ms = args[i].parse().map_err(|_| ())?;
            }
            "--interval" => {
                i += 1;
                if i >= args.len() {
                    return Err(());
                }
                opts.report_interval_ms = args[i].parse().map_err(|_| ())?;
            }
            "--batch-rows" => {
                i += 1;
                if i >= args.len() {
                    return Err(());
                }
                let n: usize = args[i].parse().map_err(|_| ())?;
                batch_single = Some(n);
                batch_multi = Some(n);
            }
            "--threads" => {
                i += 1;
                if i >= args.len() {
                    return Err(());
                }
                opts.threads = args[i].parse().map_err(|_| ())?;
            }
            "--single-only" => opts.run_multi = false,
            "--multi-only" => opts.run_single = false,
            "--no-live" => live = false,
            "--json" => json = true,
            "-q" | "--quiet" => quiet = true,
            "-V" | "--version" => {
                println!("cpuz-bench {VERSION}");
                std::process::exit(0);
            }
            "-h" | "--help" => {
                usage(&mut std::io::stdout());
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                return Err(());
            }
        }
        i += 1;
    }

    if let Some(n) = batch_single {
        opts.batch_rows_single = n.max(1);
    }
    if let Some(n) = batch_multi {
        opts.batch_rows_multi = n.max(1);
    }
    if json || quiet {
        live = false;
    }

    Ok(Cli {
        bench: opts,
        json,
        quiet,
        live,
        cpu: cpu_name(),
    })
}

/// Best-effort CPU brand string, without extra dependencies.
fn cpu_name() -> String {
    if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("model name") {
                if let Some((_, value)) = rest.split_once(':') {
                    let name = value.trim();
                    if !name.is_empty() {
                        return name.to_string();
                    }
                }
            }
        }
    }
    if cfg!(target_os = "macos") {
        if let Ok(out) = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    env::consts::ARCH.to_string()
}

/// Two-line live panel, updated in place on a TTY.
struct Panel {
    single: Option<f32>,
    multi: Option<f32>,
    lines: usize,
}

impl Panel {
    fn new() -> Self {
        Panel {
            single: None,
            multi: None,
            lines: 0,
        }
    }

    fn show(&mut self, p: &Progress) {
        match p.pass {
            Pass::Single => self.single = Some(p.score),
            Pass::Multi => self.multi = Some(p.score),
        }

        let line1 = row_line(
            "Single Thread",
            self.single,
            if p.pass == Pass::Single { Some(p) } else { None },
        );
        let line2 = row_line(
            "Multi  Thread",
            self.multi,
            if p.pass == Pass::Multi { Some(p) } else { None },
        );

        if self.lines > 0 {
            print!("\x1b[{}A", self.lines);
        }
        println!("\r\x1b[2K{line1}");
        println!("\r\x1b[2K{line2}");
        self.lines = 2;
        let _ = std::io::stdout().flush();
    }
}

fn row_line(label: &str, score: Option<f32>, active: Option<&Progress>) -> String {
    let (fraction, text) = match active {
        Some(p) if !p.done => (
            (p.elapsed_ms as f32 / p.duration_ms.max(1) as f32).clamp(0.0, 1.0),
            format!("{:>6.0}", p.score),
        ),
        _ => {
            let frac = if score.is_some() { 1.0 } else { 0.0 };
            let text = score
                .map(|s| format!("{:>6.0}", s))
                .unwrap_or_else(|| "     -".to_string());
            (frac, text)
        }
    };

    let filled = (fraction * BAR_WIDTH as f32).round() as usize;
    let mut bar = String::with_capacity(BAR_WIDTH);
    for i in 0..BAR_WIDTH {
        bar.push(if i < filled { '█' } else { '░' });
    }
    let percent = (fraction * 100.0).round() as u32;
    let time = match active {
        Some(p) => format!(
            "{:>5.1}s/{:>4.1}s",
            p.elapsed_ms as f64 / 1000.0,
            p.duration_ms as f64 / 1000.0
        ),
        None => "            ".to_string(),
    };
    format!(
        "  {label:<13} [{bar}] {percent:>3}%  score {text}  {time}"
    )
}

fn print_report(cli: &Cli, result: &BenchResult) {
    let mode = cli.bench.engine.name();
    let score = |v: f32| format!("{v:.0}");

    if cli.json {
        println!("{{");
        println!("  \"mode\": \"{mode}\",");
        println!("  \"cpu\": \"{}\",", cli.cpu);
        println!("  \"duration_ms\": {},", cli.bench.duration_ms);
        match &result.single {
            Some(r) => println!(
                "  \"single\": {{\"score\": {}, \"units\": {}, \"ms\": {}}},",
                score(r.score),
                r.units,
                r.ms
            ),
            None => println!("  \"single\": null,"),
        }
        match &result.multi {
            Some(r) => println!(
                "  \"multi\": {{\"score\": {}, \"units\": {}, \"ms\": {}, \"threads\": {}}}",
                score(r.score),
                r.units,
                r.ms,
                r.threads
            ),
            None => println!("  \"multi\": null"),
        }
        println!("}}");
        return;
    }

    if cli.quiet {
        if let Some(r) = &result.single {
            println!("CPU (Single Thread) : {:>8}", score(r.score));
        }
        if let Some(r) = &result.multi {
            println!("CPU (Multi Thread)  : {:>8}", score(r.score));
        }
        return;
    }

    println!("=== CPU-Z Benchmark ({mode}) ===");
    if let Some(r) = &result.single {
        println!(
            "CPU (Single Thread) : {:>8}   ({} px in {} ms)",
            score(r.score),
            r.units,
            r.ms
        );
    }
    if let Some(r) = &result.multi {
        println!(
            "CPU (Multi Thread)  : {:>8}   ({} px in {} ms, {} threads)",
            score(r.score),
            r.units,
            r.ms,
            r.threads
        );
    }
}

fn main() {
    let cli = match parse_args() {
        Ok(cli) => cli,
        Err(_) => {
            usage(&mut std::io::stderr());
            std::process::exit(2);
        }
    };

    if !cli.json && !cli.quiet {
        println!("cpuz-bench {VERSION} - CPU-Z benchmark ({})", cli.bench.engine.name());
        println!("CPU      : {}", cli.cpu);
        println!(
            "Settings : {} threads, {} ms/pass, batch {}/{} rows{}",
            cli.bench.threads,
            cli.bench.duration_ms,
            cli.bench.batch_rows_single,
            cli.bench.batch_rows_multi,
            if cli.live { ", live score" } else { "" }
        );
        println!();
    }

    let mut panel = Panel::new();
    let live = cli.live;
    let mut progress = |p: Progress| {
        if live {
            panel.show(&p);
        }
    };
    let result = run(&cli.bench, &mut progress);

    if cli.live {
        println!();
    }
    print_report(&cli, &result);
}
