//! Compares gungraun results with the stored instruction-count baseline.
//!
//! ```text
//! benchcheck --summaries <dir> --baseline <file> --toolchain <text>
//!            [--threshold <percent>] [--write]
//! ```
//!
//! Exit codes: 0 pass, 1 regression or missing benchmark, 2 usage or I/O
//! error, 3 the baseline came from a different toolchain (not comparable).
//! `--write` rewrites the baseline with this run's counts when the check
//! passes (or when there was no baseline yet).

use std::path::PathBuf;
use std::process::ExitCode;

use ferrojs_bench::baseline::{compare, read_summaries, Baseline};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("benchcheck: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut summaries = None;
    let mut baseline = None;
    let mut toolchain = None;
    let mut threshold = 3.0;
    let mut write = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--summaries" => summaries = Some(PathBuf::from(value()?)),
            "--baseline" => baseline = Some(PathBuf::from(value()?)),
            "--toolchain" => toolchain = Some(value()?),
            "--threshold" => {
                threshold = value()?.parse().map_err(|_| "bad --threshold")?;
            }
            "--write" => write = true,
            _ => return Err(format!("unknown argument {a}")),
        }
    }
    let summaries = summaries.ok_or("--summaries is required")?;
    let baseline_path = baseline.ok_or("--baseline is required")?;
    let toolchain = toolchain.ok_or("--toolchain is required")?;

    let counts = read_summaries(&summaries)?;
    if counts.is_empty() {
        return Err(format!(
            "no benchmark results under {}",
            summaries.display()
        ));
    }
    let new = Baseline {
        toolchain: toolchain.clone(),
        counts,
    };

    let old = match std::fs::read_to_string(&baseline_path) {
        Ok(text) => Some(Baseline::parse(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", baseline_path.display())),
    };

    let passed = match &old {
        None => {
            println!("no baseline yet at {}", baseline_path.display());
            true
        }
        Some(old) if old.toolchain != toolchain => {
            println!(
                "baseline toolchain differs; counts are not comparable\n  baseline: {}\n  this run: {toolchain}",
                old.toolchain
            );
            return Ok(ExitCode::from(3));
        }
        Some(old) => {
            let c = compare(&old.counts, &new.counts, threshold);
            for line in &c.report {
                println!("{line}");
            }
            println!(
                "{} benchmarks, {} slower than {threshold}%, {} missing, {} dropped by more than {}%",
                new.counts.len(),
                c.regressions.len(),
                c.missing.len(),
                c.collapsed.len(),
                ferrojs_bench::baseline::COLLAPSE_PCT
            );
            if !c.collapsed.is_empty() {
                println!(
                    "  a big drop usually means a benchmark stopped measuring its work; if the \
                     speedup is real, rewrite the baseline (rm conformance/bench-baseline.txt && \
                     tools/bench.sh check --write) with a decision entry"
                );
            }
            c.passed()
        }
    };

    if !passed {
        return Ok(ExitCode::from(1));
    }
    if write {
        std::fs::write(&baseline_path, new.render())
            .map_err(|e| format!("{}: {e}", baseline_path.display()))?;
        println!("wrote {}", baseline_path.display());
    }
    Ok(ExitCode::SUCCESS)
}
