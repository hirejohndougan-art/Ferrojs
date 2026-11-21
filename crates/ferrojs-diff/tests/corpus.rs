//! Runs the real corpus (`tests/diff/`) through the harness: Node.js against
//! a second Node.js process. This checks the harness end to end and that
//! every corpus file is deterministic. Skipped when `node` isn't installed.

use ferrojs_diff::{load_corpus, run};
use ferrojs_test262::engine::{Execution, External, FerroJs};
use std::path::PathBuf;
use std::time::Duration;

fn corpus_dir() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", "tests", "diff"]
        .iter()
        .collect()
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn corpus_is_not_empty() {
    let cases = load_corpus(&corpus_dir()).expect("corpus loads");
    assert!(cases.len() >= 10, "only {} corpus files", cases.len());
    assert!(cases.iter().all(|c| c.name.ends_with(".js")));
}

#[test]
fn node_matches_node_on_the_corpus() {
    if !node_available() {
        eprintln!("node not found; skipping");
        return;
    }
    let cases = load_corpus(&corpus_dir()).expect("corpus loads");
    let a = External::node(Duration::from_secs(10));
    let b = External::node(Duration::from_secs(10));
    let report = run(&cases, &a, &b);
    assert!(report.passed(), "\n{report}");
    assert_eq!(report.matched(), cases.len());

    // Only files under `errors/` may end with an uncaught error; anywhere
    // else one means the file is broken (e.g. it uses a host API the
    // reference realm doesn't have) and would silently test less.
    let mut errors = Vec::new();
    for case in &cases {
        let Execution::Completed { error, printed } = a.run_case(case) else {
            panic!("{} did not complete", case.name);
        };
        match error {
            Some(t) => {
                assert!(
                    case.name.starts_with("errors/"),
                    "{} throws {}: {}",
                    case.name,
                    t.name,
                    t.message
                );
                errors.push((t.name, t.phase));
            }
            None => assert!(!printed.is_empty(), "{} prints nothing", case.name),
        }
    }
    assert!(errors.iter().any(|(n, _)| n == "SyntaxError"), "{errors:?}");
    assert!(errors.iter().any(|(n, _)| n == "TypeError"), "{errors:?}");
}

#[test]
fn ferrojs_runs_only_files_with_early_errors_until_it_runs_javascript() {
    let cases = load_corpus(&corpus_dir()).expect("corpus loads");
    // The reference is never consulted for skipped files. FerroJS can parse
    // (task 1.5) but not run code, so only files it rejects before running
    // are compared: the syntax-error case.
    let report = run(&cases, &FerroJs, &FerroJs);
    assert!(report.passed());
    assert_eq!(report.skipped(), cases.len() - 1);
}

trait RunCase {
    fn run_case(&self, case: &ferrojs_diff::Case) -> Execution;
}
impl<E: ferrojs_test262::engine::Engine> RunCase for E {
    fn run_case(&self, case: &ferrojs_diff::Case) -> Execution {
        self.run(&case.job())
    }
}
