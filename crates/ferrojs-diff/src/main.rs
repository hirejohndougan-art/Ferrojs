//! `jsdiff` command-line tool.
//!
//! ```text
//! jsdiff [options] <file or directory>...
//!
//!   --engine ferrojs|node   engine under test (default: ferrojs)
//!   --timeout <secs>        per-file timeout for external engines (default: 10)
//! ```
//!
//! Runs every `.js` file on the engine and on Node.js (the reference) and
//! exits with status 1 if any file's printed output or uncaught error
//! differs. `--engine node` compares Node.js with a second, independent Node.js
//! process: it checks the harness and that every corpus file is
//! deterministic.

use ferrojs_diff::{load_corpus, run};
use ferrojs_test262::engine::{Engine, External, FerroJs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let mut engine_name = "ferrojs".to_string();
    let mut timeout = Duration::from_secs(10);
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--engine" => match args.next() {
                Some(v) => engine_name = v,
                None => return usage("--engine needs a value"),
            },
            "--timeout" => match args.next().and_then(|v| v.parse().ok()) {
                Some(s) => timeout = Duration::from_secs(s),
                None => return usage("--timeout needs seconds"),
            },
            "-h" | "--help" => return usage(""),
            other if other.starts_with("--") => return usage(&format!("unknown option {other}")),
            path => roots.push(PathBuf::from(path)),
        }
    }
    if roots.is_empty() {
        return usage("give at least one file or directory");
    }

    let engine: Box<dyn Engine> = match engine_name.as_str() {
        "ferrojs" => Box::new(FerroJs),
        "node" => Box::new(External::node(timeout)),
        other => {
            return usage(&format!(
                "unknown engine {other} (expected ferrojs or node)"
            ))
        }
    };
    let reference = External::node(timeout);

    let mut cases = Vec::new();
    for root in &roots {
        match load_corpus(root) {
            Ok(c) => cases.extend(c),
            Err(e) => {
                eprintln!("could not read {}: {e}", root.display());
                return ExitCode::from(2);
            }
        }
    }
    if cases.is_empty() {
        eprintln!("no .js files found");
        return ExitCode::from(2);
    }

    let report = run(&cases, engine.as_ref(), &reference);
    println!("Differential test: {} vs node", engine.name());
    print!("{report}");
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn usage(problem: &str) -> ExitCode {
    if !problem.is_empty() {
        eprintln!("{problem}");
    }
    eprintln!("usage: jsdiff [--engine ferrojs|node] [--timeout <secs>] <file or directory>...");
    ExitCode::from(2)
}
