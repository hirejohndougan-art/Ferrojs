//! Differential testing (decision D-013).
//!
//! Each file in a corpus (`tests/diff/` at the repository root) is a plain
//! script that reports what it computes with `print(...)`. The harness runs
//! every file on the engine under test and on a reference engine (Node.js,
//! through the same driver the Test262 runner uses) and compares:
//!
//! - the printed lines, exactly and in order;
//! - the uncaught error, if any: its constructor name and phase.
//!
//! Error messages are not compared: engines word them differently and the
//! specification leaves them implementation-defined.
//!
//! An engine that reports `Unsupported` is counted as skipped, never as a
//! match. A crash or timeout on either side is a difference.

use ferrojs_test262::engine::{Engine, Execution, Thrown};
use ferrojs_test262::job::{Job, Mode};
use std::fmt;
use std::path::{Path, PathBuf};

/// One corpus file.
#[derive(Clone, Debug)]
pub struct Case {
    /// Path relative to the corpus root, with `/` separators.
    pub name: String,
    pub path: PathBuf,
    pub source: String,
}

impl Case {
    /// The job handed to an engine: the file runs once, unmodified, as a
    /// non-module script with no harness (Test262's `raw` mode).
    pub fn job(&self) -> Job {
        Job {
            id: self.name.clone(),
            rel_path: self.name.clone(),
            path: self.path.clone(),
            mode: Mode::Raw,
            prelude: String::new(),
            body: self.source.clone(),
            negative: None,
            is_async: false,
            features: Vec::new(),
        }
    }
}

/// Every `.js` file under `root` (or `root` itself if it is a file), sorted
/// by name so runs are reproducible.
pub fn load_corpus(root: &Path) -> std::io::Result<Vec<Case>> {
    let mut cases = Vec::new();
    for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
        let entry = entry.map_err(std::io::Error::other)?;
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension() != Some(std::ffi::OsStr::new("js")) {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        let name = if rel.as_os_str().is_empty() {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            rel.to_string_lossy().replace('\\', "/")
        };
        cases.push(Case {
            name,
            path: path.to_path_buf(),
            source: std::fs::read_to_string(path)?,
        });
    }
    Ok(cases)
}

/// The result of comparing one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Match,
    /// The engine under test can't run this file yet.
    Skipped(String),
    Differ(String),
}

fn describe_error(e: &Option<Thrown>) -> String {
    match e {
        None => "no uncaught error".into(),
        Some(t) => {
            let phase = match t.phase {
                Some(p) => format!("{p:?}").to_lowercase(),
                None => "unknown".into(),
            };
            format!("uncaught {} ({phase} phase): {}", t.name, t.message)
        }
    }
}

fn same_error(a: &Option<Thrown>, b: &Option<Thrown>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.name == b.name && a.phase == b.phase,
        _ => false,
    }
}

fn first_difference(a: &[String], b: &[String]) -> String {
    let i = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let show = |v: &[String]| match v.get(i) {
        Some(line) => format!("{line:?}"),
        None => "<end of output>".into(),
    };
    format!(
        "printed output differs at line {}: engine {}, reference {}",
        i + 1,
        show(a),
        show(b)
    )
}

/// Compares the engine's execution with the reference's.
pub fn compare(engine: &Execution, reference: &Execution) -> Outcome {
    let (ep, ee) = match engine {
        Execution::Unsupported(why) => return Outcome::Skipped(why.clone()),
        Execution::Timeout => return Outcome::Differ("engine timed out".into()),
        Execution::Crash(why) => return Outcome::Differ(format!("engine crashed: {why}")),
        Execution::Completed { error, printed } => (printed, error),
    };
    let (rp, re) = match reference {
        Execution::Completed { error, printed } => (printed, error),
        Execution::Unsupported(why) => {
            return Outcome::Differ(format!("reference can't run this file: {why}"))
        }
        Execution::Timeout => return Outcome::Differ("reference timed out".into()),
        Execution::Crash(why) => return Outcome::Differ(format!("reference crashed: {why}")),
    };
    if ep != rp {
        return Outcome::Differ(first_difference(ep, rp));
    }
    if !same_error(ee, re) {
        return Outcome::Differ(format!(
            "engine: {}; reference: {}",
            describe_error(ee),
            describe_error(re)
        ));
    }
    Outcome::Match
}

/// The outcome for every file, in corpus order.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub results: Vec<(String, Outcome)>,
}

impl Report {
    fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.results.iter().filter(|(_, o)| f(o)).count()
    }
    pub fn matched(&self) -> usize {
        self.count(|o| *o == Outcome::Match)
    }
    pub fn skipped(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Skipped(_)))
    }
    pub fn differences(&self) -> impl Iterator<Item = (&str, &str)> {
        self.results.iter().filter_map(|(n, o)| match o {
            Outcome::Differ(why) => Some((n.as_str(), why.as_str())),
            _ => None,
        })
    }
    pub fn differ(&self) -> usize {
        self.differences().count()
    }
    pub fn passed(&self) -> bool {
        self.differ() == 0
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, why) in self.differences() {
            writeln!(f, "  DIFFER {name}: {why}")?;
        }
        writeln!(
            f,
            "  {} files: {} match, {} differ, {} skipped (engine can't run them yet)",
            self.results.len(),
            self.matched(),
            self.differ(),
            self.skipped()
        )
    }
}

/// Runs every case on both engines and compares the results.
pub fn run(cases: &[Case], engine: &dyn Engine, reference: &dyn Engine) -> Report {
    let results = cases
        .iter()
        .map(|case| {
            let job = case.job();
            let e = engine.run(&job);
            // No need to ask the reference about files the engine skipped.
            let outcome = match e {
                Execution::Unsupported(why) => Outcome::Skipped(why),
                e => compare(&e, &reference.run(&job)),
            };
            (case.name.clone(), outcome)
        })
        .collect();
    Report { results }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrojs_test262::metadata::Phase;

    fn done(printed: &[&str], error: Option<(&str, Phase)>) -> Execution {
        Execution::Completed {
            printed: printed.iter().map(|s| s.to_string()).collect(),
            error: error.map(|(name, phase)| Thrown {
                phase: Some(phase),
                name: name.into(),
                message: "some message".into(),
            }),
        }
    }

    #[test]
    fn identical_runs_match() {
        let a = done(&["1", "two"], None);
        assert_eq!(compare(&a, &a.clone()), Outcome::Match);
    }

    #[test]
    fn printed_lines_must_match_exactly() {
        let a = done(&["1", "2"], None);
        let b = done(&["1", "3"], None);
        match compare(&a, &b) {
            Outcome::Differ(why) => assert!(why.contains("line 2"), "{why}"),
            o => panic!("expected a difference, got {o:?}"),
        }
        let shorter = done(&["1"], None);
        match compare(&shorter, &a) {
            Outcome::Differ(why) => assert!(why.contains("<end of output>"), "{why}"),
            o => panic!("expected a difference, got {o:?}"),
        }
    }

    #[test]
    fn errors_compare_by_name_and_phase_not_message() {
        let mut a = done(&[], Some(("TypeError", Phase::Runtime)));
        if let Execution::Completed { error: Some(t), .. } = &mut a {
            t.message = "different wording".into();
        }
        let b = done(&[], Some(("TypeError", Phase::Runtime)));
        assert_eq!(compare(&a, &b), Outcome::Match);

        let other_name = done(&[], Some(("RangeError", Phase::Runtime)));
        assert!(matches!(compare(&other_name, &b), Outcome::Differ(_)));
        let other_phase = done(&[], Some(("TypeError", Phase::Parse)));
        assert!(matches!(compare(&other_phase, &b), Outcome::Differ(_)));
        let none = done(&[], None);
        assert!(matches!(compare(&none, &b), Outcome::Differ(_)));
        assert!(matches!(compare(&b, &none), Outcome::Differ(_)));
    }

    #[test]
    fn unsupported_is_skipped_but_failures_differ() {
        let ok = done(&[], None);
        assert!(matches!(
            compare(&Execution::Unsupported("no".into()), &ok),
            Outcome::Skipped(_)
        ));
        for bad in [Execution::Timeout, Execution::Crash("boom".into())] {
            assert!(matches!(compare(&bad, &ok), Outcome::Differ(_)));
            assert!(matches!(compare(&ok, &bad), Outcome::Differ(_)));
        }
        assert!(matches!(
            compare(&ok, &Execution::Unsupported("no".into())),
            Outcome::Differ(_)
        ));
    }

    /// A fake engine that answers from a fixed function.
    struct Fake(fn(&Job) -> Execution);
    impl Engine for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn run(&self, job: &Job) -> Execution {
            (self.0)(job)
        }
    }

    #[test]
    fn run_reports_each_file() {
        let cases: Vec<Case> = ["a.js", "b.js", "c.js"]
            .iter()
            .map(|n| Case {
                name: n.to_string(),
                path: PathBuf::from(n),
                source: format!("print('{n}')"),
            })
            .collect();
        let reference = Fake(|job| done(&[&job.body], None));
        let engine = Fake(|job| match job.rel_path.as_str() {
            "a.js" => done(&[&job.body], None),
            "b.js" => done(&["wrong"], None),
            _ => Execution::Unsupported("not yet".into()),
        });
        let report = run(&cases, &engine, &reference);
        assert_eq!(
            (report.matched(), report.differ(), report.skipped()),
            (1, 1, 1)
        );
        assert!(!report.passed());
        assert_eq!(report.differences().next().unwrap().0, "b.js");
        assert!(report.to_string().contains("DIFFER b.js"));
    }

    #[test]
    fn jobs_are_raw_scripts() {
        let case = Case {
            name: "x.js".into(),
            path: PathBuf::from("x.js"),
            source: "print(1)".into(),
        };
        let job = case.job();
        assert_eq!(job.mode, Mode::Raw);
        assert_eq!(job.script_source(), "print(1)");
    }
}
