//! CLI front-end with a CPU-Z style live score panel.
//!
//! While a pass runs the score is re-computed as `completed_units / elapsed_ms`
//! every `--interval` milliseconds, so it appears immediately and settles
//! towards the final number instead of only showing up at the end.

use std::env;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use cpuz_bench::bench::{run, BenchOptions, BenchResult, Pass, Progress};
use cpuz_bench::kernel::Engine;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BAR_WIDTH: usize = 24;

/// On x86_64 this crate reproduces CPU-Z's own numbers (872 vs 872 on an
/// i7-13700KF against the published leaderboard), so there is nothing to
/// gain from running CPUID's binary. On aarch64 the replica is an LLVM build
/// of a verified-identical algorithm but runs measurably faster than CPUID's
/// MSVC build, so the CLI prefers the official kernel when it is available.
#[cfg(target_arch = "x86_64")]
const PLATFORM_NOTE: Option<&str> = None;
#[cfg(target_arch = "aarch64")]
const PLATFORM_NOTE: Option<&str> = Some(
    "Note: aarch64 build — this crate's LLVM build renders the same, verified-identical\n        \
     kernel but runs faster than CPUID's MSVC build, so the replica score is not the\n        \
     score CPU-Z reports. Pass --official <cpuz_arm64.exe> (or put the file in the\n        \
     cache / set CPUZ_ARM64_EXE) to score with CPU-Z's own code.",
);
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const PLATFORM_NOTE: Option<&str> = Some(
    "Note: non-x86 build — 17.01 is an x86 SSE scalar workload, so this score is\n        \
     not comparable to CPU-Z's x86 reference scores.",
);

struct Cli {
    bench: BenchOptions,
    json: bool,
    quiet: bool,
    live: bool,
    cpu: String,
    /// `--official <path>`: run CPUID's own ARM64 kernel instead of ours.
    official_path: Option<PathBuf>,
    /// `--no-official`: always use the built-in engine.
    use_official: bool,
    /// `--compare`: also run the built-in engine and print the delta.
    compare: bool,
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
  --interval <ms>    Live panel repaint interval (default 100, 0 = off)
  --batch-rows <n>   Rows per dispatched job (default: single 8, multi 16)
  --threads <n>      Worker threads for the multi-thread pass (default: all CPUs)
  --single-only      Run only the single-thread pass
  --multi-only       Run only the multi-thread pass
  --official <path>  aarch64 only: score with CPUID's own cpuz_arm64.exe kernel
                     (default: $CPUZ_ARM64_EXE, else the shared cache path)
  --no-official      aarch64 only: always use the built-in engine
  --compare          Also run the built-in engine and print the difference
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
    let mut official_path: Option<PathBuf> = None;
    let mut use_official = true;
    let mut compare = false;

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
            "--official" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--official needs the path to cpuz_arm64.exe");
                    return Err(());
                }
                official_path = Some(PathBuf::from(&args[i]));
            }
            "--no-official" => use_official = false,
            "--compare" => compare = true,
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
        official_path,
        use_official,
        compare,
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
/// Ookla's transfer bar spins `-`, `\`, `|`, `/` at the head of the fill.
const SPINNER: [char; 4] = ['-', '\\', '|', '/'];
/// How long each spinner frame lasts. speedtest advances its spinner once per
/// progress repaint (~10 Hz); driving ours off the clock keeps the rotation at
/// that rate even when the panel is repainted less often than that.
const SPIN_MS: u64 = 100;

struct Panel {
    single: Option<f32>,
    multi: Option<f32>,
    lines: usize,
    started: std::time::Instant,
}

impl Panel {
    fn new() -> Self {
        Panel {
            single: None,
            multi: None,
            lines: 0,
            started: std::time::Instant::now(),
        }
    }

    fn show(&mut self, p: &Progress) {
        match p.pass {
            Pass::Single => self.single = Some(p.score),
            Pass::Multi => self.multi = Some(p.score),
        }

        let frame = (self.started.elapsed().as_millis() as u64 / SPIN_MS) as usize;
        let spin = SPINNER[frame % SPINNER.len()];

        let line1 = row_line(
            "Single Thread",
            self.single,
            if p.pass == Pass::Single { Some(p) } else { None },
            spin,
        );
        let line2 = row_line(
            "Multi  Thread",
            self.multi,
            if p.pass == Pass::Multi { Some(p) } else { None },
            spin,
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

/// How far along a row's bar is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bar {
    /// Not started (or not this pass): an empty track, no cursor.
    Idle,
    /// Running: `=` up to the cursor, spinning head, spaces behind.
    Running,
    /// Finished: filled edge to edge.
    Done,
}

fn row_line(label: &str, score: Option<f32>, active: Option<&Progress>, spin: char) -> String {
    let (state, fraction, text) = match active {
        Some(p) if !p.done => (
            Bar::Running,
            (p.elapsed_ms as f32 / p.duration_ms.max(1) as f32).clamp(0.0, 1.0),
            format!("{:>6.0}", p.score),
        ),
        _ => match score {
            Some(s) => (Bar::Done, 1.0, format!("{s:>6.0}")),
            None => (Bar::Idle, 0.0, "     -".to_string()),
        },
    };

    // Ookla/speedtest layout: `=` for the completed part, a spinning cursor at
    // its head, then spaces.
    let filled = (fraction * BAR_WIDTH as f32).round() as usize;
    let mut bar = String::with_capacity(BAR_WIDTH);
    match state {
        Bar::Idle => bar.extend(std::iter::repeat(' ').take(BAR_WIDTH)),
        Bar::Done => bar.extend(std::iter::repeat('=').take(BAR_WIDTH)),
        Bar::Running => {
            for i in 0..BAR_WIDTH {
                bar.push(if i < filled {
                    '='
                } else if i == filled {
                    spin
                } else {
                    ' '
                });
            }
        }
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

fn print_header(cli: &Cli, source: &str, show_platform_note: bool) {
    if cli.json || cli.quiet {
        return;
    }
    println!(
        "cpuz-bench {VERSION} - CPU-Z benchmark ({})",
        cli.bench.engine.name()
    );
    println!("CPU      : {}", cli.cpu);
    println!("Source   : {source}");
    println!(
        "Settings : {} threads, {} ms/pass, batch {}/{} rows{}",
        cli.bench.threads,
        cli.bench.duration_ms,
        cli.bench.batch_rows_single,
        cli.bench.batch_rows_multi,
        if cli.live { ", live score" } else { "" }
    );
    if show_platform_note {
        if let Some(note) = PLATFORM_NOTE {
            println!("{note}");
        }
    }
    println!();
}

fn delta(ours: f32, reference: f32) -> String {
    if reference <= 0.0 {
        return "n/a".to_string();
    }
    format!("{:+.1}%", (ours - reference) / reference * 100.0)
}

fn print_report(cli: &Cli, result: &BenchResult, source: &str, replica: Option<&BenchResult>) {
    let mode = cli.bench.engine.name();
    let score = |v: f32| format!("{v:.0}");

    if cli.json {
        println!("{{");
        println!("  \"mode\": \"{mode}\",");
        println!("  \"cpu\": \"{}\",", cli.cpu);
        println!("  \"arch\": \"{}\",", env::consts::ARCH);
        println!("  \"source\": \"{source}\",");
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
                "  \"multi\": {{\"score\": {}, \"units\": {}, \"ms\": {}, \"threads\": {}}},",
                score(r.score),
                r.units,
                r.ms,
                r.threads
            ),
            None => println!("  \"multi\": null,"),
        }
        match replica {
            None => println!("  \"replica\": null"),
            Some(rep) => {
                let s = rep.single.as_ref().map(|r| score(r.score)).unwrap_or_else(|| "null".into());
                let m = rep.multi.as_ref().map(|r| score(r.score)).unwrap_or_else(|| "null".into());
                println!("  \"replica\": {{\"single\": {s}, \"multi\": {m}}}");
            }
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
    let source_line = if source == "official" {
        "official kernel"
    } else {
        "built-in engine"
    };
    if let Some(r) = &result.single {
        println!(
            "CPU (Single Thread) : {:>8}   ({} px in {} ms, {source_line})",
            score(r.score),
            r.units,
            r.ms
        );
    }
    if let Some(r) = &result.multi {
        println!(
            "CPU (Multi Thread)  : {:>8}   ({} px in {} ms, {} threads, {source_line})",
            score(r.score),
            r.units,
            r.ms,
            r.threads
        );
    }
    if let Some(rep) = replica {
        let s = rep
            .single
            .as_ref()
            .map(|r| (score(r.score), delta(r.score, result.single_score())))
            .unwrap_or_else(|| ("-".into(), "-".into()));
        let m = rep
            .multi
            .as_ref()
            .map(|r| (score(r.score), delta(r.score, result.multi_score())))
            .unwrap_or_else(|| ("-".into(), "-".into()));
        println!(
            "This crate (LLVM)   : {:>8} / {:>6}   and {:>8} / {:>6}",
            s.0, s.1, m.0, m.1
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn run_official(cli: &Cli) -> Result<(), Option<String>> {
    use cpuz_bench::official;

    // Only complain about a missing executable when the user actually asked for
    // one; otherwise the header/README note is explanation enough.
    let explicit = cli.official_path.is_some() || std::env::var_os("CPUZ_ARM64_EXE").is_some();
    let path = match official::locate(cli.official_path.as_deref()) {
        Some(p) => p,
        None if !explicit => return Err(None),
        None => {
            return Err(Some(format!(
                "cpuz_arm64.exe not found — download\n        \
                 https://download.cpuid.com/cpu-z/arm64/cpuz-arm64_1.06.zip\n        \
                 and put the exe at {} or point CPUZ_ARM64_EXE at it",
                official::default_cache_path().display()
            )))
        }
    };
    let kernel = official::Kernel::load(&path).map_err(Some)?;

    let source = format!(
        "official cpuz_arm64.exe {} kernel ({})",
        kernel.version,
        path.display()
    );
    print_header(cli, &source, false);

    let mut panel = Panel::new();
    let live = cli.live;
    let mut sink = |p: Progress| {
        if live {
            panel.show(&p);
        }
    };
    let result = official::run(&kernel, &cli.bench, &mut sink);
    if cli.live {
        println!();
    }

    let replica = if cli.compare {
        let mut silent = |_: Progress| {};
        Some(run(&cli.bench, &mut silent))
    } else {
        None
    };
    print_report(cli, &result, "official", replica.as_ref());
    Ok(())
}

fn run_replica(cli: &Cli) {
    print_header(cli, "built-in engine (this crate, LLVM build)", true);

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
    print_report(cli, &result, "builtin", None);
}

fn main() {
    let cli = match parse_args() {
        Ok(cli) => cli,
        Err(_) => {
            usage(&mut std::io::stderr());
            std::process::exit(2);
        }
    };

    #[cfg(not(target_arch = "aarch64"))]
    if cli.official_path.is_some() {
        eprintln!(
            "cpuz-bench: --official runs CPUID's Windows ARM64 kernel, so it is only\n            \
             available on aarch64. On x86_64 the built-in engine already matches\n            \
             CPU-Z's published scores."
        );
        std::process::exit(2);
    }

    #[cfg(target_arch = "aarch64")]
    if cli.use_official {
        match run_official(&cli) {
            Ok(()) => return,
            Err(None) => {}
            Err(Some(err)) => {
                for line in err.lines() {
                    eprintln!("cpuz-bench: {line}");
                }
                eprintln!("cpuz-bench: falling back to the built-in engine");
                if !cli.quiet && !cli.json {
                    eprintln!();
                }
            }
        }
    }

    run_replica(&cli);
}
