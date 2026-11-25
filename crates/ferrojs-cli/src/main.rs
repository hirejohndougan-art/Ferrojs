//! The `ferrojs` command (task 1.9): runs a script and prints uncaught
//! errors.
//!
//! ```text
//! ferrojs <file.js>        run a script file
//! ferrojs -e '<source>'    run source text
//! ferrojs -- <file.js>     a file whose name starts with `-`
//! ```
//!
//! Scripts get one host function, `print(value)`, which writes
//! `String(value)` and a newline to standard output (the convention of the
//! Test262 and differential-test hosts). An uncaught error goes to
//! standard error; the exit status is 0 on success, 1 for an uncaught
//! error or a syntax error, 2 for usage errors and unreadable files, and 3
//! for scripts FerroJS can't run yet.

use std::io::Write;
use std::process::ExitCode;

use ferrojs::{Engine, Error, JsValue};

fn usage() -> ExitCode {
    eprintln!("usage: ferrojs <file.js>\n       ferrojs -e '<source>'");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (name, source) = match args.as_slice() {
        [flag, source] if flag == "-e" => ("<command line>".to_owned(), Some(source.clone())),
        [flag] if flag == "-h" || flag == "--help" => {
            println!("usage: ferrojs <file.js>\n       ferrojs -e '<source>'");
            return ExitCode::SUCCESS;
        }
        [path] if !path.starts_with('-') => read(path),
        [dashes, path] if dashes == "--" => read(path),
        _ => return usage(),
    };
    let (name, source) = match (name, source) {
        (name, Some(source)) => (name, source),
        (_, None) => return ExitCode::from(2),
    };
    run(&name, &source)
}

/// The file at `path` (name and text), or `None` after reporting why not.
fn read(path: &str) -> (String, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(s) => (path.to_owned(), Some(s)),
        Err(e) => {
            eprintln!("ferrojs: can't read {path}: {e}");
            (path.to_owned(), None)
        }
    }
}

fn run(name: &str, source: &str) -> ExitCode {
    let mut engine = match Engine::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ferrojs: {e}");
            return ExitCode::from(2);
        }
    };
    let installed = engine.register("print", |args| {
        let line = args.first().map_or_else(String::new, JsValue::to_string);
        let mut out = std::io::stdout().lock();
        // A closed pipe isn't the script's fault: ignore write errors.
        let _ = writeln!(out, "{line}");
        Ok(JsValue::Undefined)
    });
    if let Err(e) = installed {
        eprintln!("ferrojs: {e}");
        return ExitCode::from(2);
    }
    let result = engine.run(source);
    let _ = std::io::stdout().flush();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e @ Error::Unsupported(_)) => {
            eprintln!("{name}: {e}");
            ExitCode::from(3)
        }
        Err(e) => {
            eprintln!("{name}: {e}");
            ExitCode::from(1)
        }
    }
}
