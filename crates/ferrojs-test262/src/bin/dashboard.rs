//! `dashboard` command-line tool: writes the progress dashboard.
//!
//! ```text
//! dashboard --report <test262 --out JSON> --bench <bench baseline>
//!           --source <revision label> --out <markdown file>
//! ```
//!
//! `tools/dashboard.sh` runs it with the right inputs.

use ferrojs_bench::baseline::Baseline;
use ferrojs_test262::dashboard::{render, Results};
use std::path::PathBuf;
use std::process::ExitCode;

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut report, mut bench, mut source, mut out) = (None, None, None, None);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--report" => report = Some(PathBuf::from(value()?)),
            "--bench" => bench = Some(PathBuf::from(value()?)),
            "--source" => source = Some(value()?),
            "--out" => out = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown option {other}")),
        }
    }
    let usage = "usage: dashboard --report <json> --bench <file> --source <label> --out <file>";
    let (report, bench, source, out) = match (report, bench, source, out) {
        (Some(r), Some(b), Some(s), Some(o)) => (r, b, s, o),
        _ => return Err(usage.into()),
    };
    let read =
        |p: &PathBuf| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let results = Results::parse(&read(&report)?)?;
    let bench = Baseline::parse(&read(&bench)?).map_err(|e| format!("{}: {e}", bench.display()))?;
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&out, render(&results, &bench, &source))
        .map_err(|e| format!("{}: {e}", out.display()))?;
    println!("dashboard written to {}", out.display());
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
