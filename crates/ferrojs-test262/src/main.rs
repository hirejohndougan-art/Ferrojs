//! `test262` command-line tool.
//!
//! ```text
//! test262 --suite <test262 checkout> [options]
//!
//!   --engine ferrojs|node   engine to test (default: ferrojs)
//!   --filter <prefix>       only tests under this path, e.g. built-ins/Array (repeatable)
//!   --no-intl               skip intl402 tests
//!   --threads <n>           parallel runs (default: all cores)
//!   --timeout <secs>        per-test timeout for external engines (default: 10)
//!   --out <file>            write the full JSON report here
//!   --baseline <file>       fail if any test listed here no longer passes
//!   --write-baseline <file> save the passing tests as the new baseline
//!   --show-failures <n>     print the first n failures (default: 0)
//! ```

use ferrojs_test262::engine::{Engine, External, FerroJs};
use ferrojs_test262::job::Selection;
use ferrojs_test262::report;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

struct Args {
    suite: PathBuf,
    engine: String,
    selection: Selection,
    threads: Option<usize>,
    timeout: Duration,
    out: Option<PathBuf>,
    baseline: Option<PathBuf>,
    write_baseline: Option<PathBuf>,
    show_failures: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut a = Args {
        suite: PathBuf::new(),
        engine: "ferrojs".into(),
        selection: Selection::default(),
        threads: None,
        timeout: Duration::from_secs(10),
        out: None,
        baseline: None,
        write_baseline: None,
        show_failures: 0,
    };
    let mut suite = None;
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--suite" => suite = Some(PathBuf::from(value()?)),
            "--engine" => a.engine = value()?,
            "--filter" => a.selection.prefixes.push(value()?),
            "--no-intl" => a.selection.include_intl = false,
            "--threads" => a.threads = Some(value()?.parse().map_err(|_| "--threads needs a number")?),
            "--timeout" => a.timeout = Duration::from_secs(value()?.parse().map_err(|_| "--timeout needs seconds")?),
            "--out" => a.out = Some(PathBuf::from(value()?)),
            "--baseline" => a.baseline = Some(PathBuf::from(value()?)),
            "--write-baseline" => a.write_baseline = Some(PathBuf::from(value()?)),
            "--show-failures" => a.show_failures = value()?.parse().map_err(|_| "--show-failures needs a number")?,
            "-h" | "--help" => return Err("usage: test262 --suite <dir> [--engine ferrojs|node] [--filter <prefix>] [--out <file>] [--baseline <file>] [--write-baseline <file>]".into()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    a.suite = suite.ok_or("--suite <test262 checkout> is required")?;
    Ok(a)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if let Some(n) = args.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
            .expect("thread pool is built once");
    }

    let engine: Box<dyn Engine> = match args.engine.as_str() {
        "ferrojs" => Box::new(FerroJs),
        "node" => Box::new(External::node(args.timeout)),
        other => {
            eprintln!("unknown engine {other} (expected ferrojs or node)");
            return ExitCode::from(2);
        }
    };

    let commit = std::process::Command::new("git")
        .args(["-C", &args.suite.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());

    let started = Instant::now();
    let report =
        match ferrojs_test262::run_suite(&args.suite, &args.selection, engine.as_ref(), commit) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("could not run the suite at {}: {e}", args.suite.display());
                return ExitCode::from(2);
            }
        };
    print!("{}", report.summary());
    println!("  time: {:.1}s", started.elapsed().as_secs_f64());

    for f in report.failures.iter().take(args.show_failures) {
        println!("  FAIL {}: {}", f.id, f.reason);
    }

    if let Some(path) = &args.out {
        let json = serde_json::to_string_pretty(&report).expect("report serializes");
        if let Err(e) = std::fs::write(path, json) {
            eprintln!("could not write {}: {e}", path.display());
            return ExitCode::from(2);
        }
    }
    if let Some(path) = &args.write_baseline {
        if let Err(e) = std::fs::write(path, report::format_baseline(&report.passing)) {
            eprintln!("could not write {}: {e}", path.display());
            return ExitCode::from(2);
        }
    }

    if let Some(path) = &args.baseline {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("could not read baseline {}: {e}", path.display());
                return ExitCode::from(2);
            }
        };
        let baseline = report::parse_baseline(&text);
        let regressions = report.regressions(&baseline);
        println!(
            "  baseline: {} passing before, {} newly passing, {} regressions",
            baseline.len(),
            report.newly_passing(&baseline),
            regressions.len()
        );
        if !regressions.is_empty() {
            for id in regressions.iter().take(50) {
                println!("  REGRESSION {id}");
            }
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
