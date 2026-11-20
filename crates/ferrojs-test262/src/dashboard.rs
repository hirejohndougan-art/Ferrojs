//! The progress dashboard: Test262 pass counts per directory and the
//! benchmark instruction counts, as one Markdown page
//! (`docs/reports/dashboard.md`, written by `tools/dashboard.sh`).
//!
//! The input is the JSON report the `test262` tool writes with `--out`, plus
//! the stored benchmark baseline. The output depends only on those inputs and
//! the source label, so regenerating it without changes gives the same file.

use crate::report::Counts;
use ferrojs_bench::baseline::Baseline;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The parts of a `test262 --out` report the dashboard uses.
#[derive(Deserialize, Debug)]
pub struct Results {
    pub engine: String,
    pub test262_commit: Option<String>,
    pub total: Counts,
    pub skipped_files: usize,
    /// Keyed by the first two path components, e.g. `built-ins/Array`.
    pub by_area: BTreeMap<String, Counts>,
}

impl Results {
    pub fn parse(json: &str) -> Result<Results, String> {
        serde_json::from_str(json).map_err(|e| format!("bad test262 report: {e}"))
    }

    /// Counts per top-level directory (`built-ins`, `language`, ...).
    pub fn by_top(&self) -> BTreeMap<&str, Counts> {
        let mut tops: BTreeMap<&str, Counts> = BTreeMap::new();
        for (area, c) in &self.by_area {
            let top = area.split('/').next().unwrap_or(area);
            tops.entry(top).or_default().merge(c);
        }
        tops
    }

    /// Counts per directory, two levels deep. Tests that sit directly in a
    /// top-level directory (e.g. `harness/assert-false.js`, whose report area
    /// is the file itself) are counted under that directory.
    pub fn by_directory(&self) -> BTreeMap<&str, Counts> {
        let mut dirs: BTreeMap<&str, Counts> = BTreeMap::new();
        for (area, c) in &self.by_area {
            let dir = match area.split_once('/') {
                Some((top, rest)) if rest.ends_with(".js") => top,
                _ => area.as_str(),
            };
            dirs.entry(dir).or_default().merge(c);
        }
        dirs
    }
}

fn row(s: &mut String, name: &str, c: &Counts) {
    writeln!(
        s,
        "| {name} | {} | {} | {:.2}% |",
        c.passed,
        c.total(),
        c.percent()
    )
    .unwrap();
}

/// Renders the dashboard. `source` names the FerroJS revision measured,
/// e.g. `698669f (2026-11-19)`.
pub fn render(results: &Results, bench: &Baseline, source: &str) -> String {
    let mut s = String::from("# FerroJS progress dashboard\n\n");
    s.push_str("Written by `tools/dashboard.sh`; don't edit by hand.\n\n");
    let commit = results.test262_commit.as_deref().unwrap_or("unknown");
    let commit = commit.get(..12).unwrap_or(commit);
    writeln!(
        s,
        "- FerroJS revision: {source}\n- Engine: {}\n- Test262 suite commit: {commit}\n",
        results.engine
    )
    .unwrap();

    s.push_str("## Test262\n\n");
    let t = &results.total;
    writeln!(
        s,
        "**{} of {} runs pass ({:.2}%).** {} fail, {} were not run, {} files skipped.\n",
        t.passed,
        t.total(),
        t.percent(),
        t.failed,
        t.not_run,
        results.skipped_files
    )
    .unwrap();

    s.push_str("| Directory | Passing | Runs | Pass rate |\n| --- | ---: | ---: | ---: |\n");
    let tops = results.by_top();
    for (top, c) in &tops {
        row(&mut s, top, c);
    }
    row(&mut s, "**all**", t);

    for top in tops.keys() {
        writeln!(s, "\n### {top}\n").unwrap();
        s.push_str("| Directory | Passing | Runs | Pass rate |\n| --- | ---: | ---: | ---: |\n");
        let prefix = format!("{top}/");
        for (area, c) in &results.by_directory() {
            if area == top || area.starts_with(&prefix) {
                row(&mut s, area, c);
            }
        }
    }

    s.push_str("\n## Benchmarks\n\n");
    if bench.counts.is_empty() {
        s.push_str("No benchmarks yet.\n");
    } else {
        writeln!(
            s,
            "Callgrind instruction counts from `conformance/bench-baseline.txt` \
             (lower is better; toolchain: {}).\n",
            if bench.toolchain.is_empty() {
                "unknown"
            } else {
                &bench.toolchain
            }
        )
        .unwrap();
        s.push_str("| Benchmark | Instructions |\n| --- | ---: |\n");
        for (id, n) in &bench.counts {
            writeln!(s, "| {id} | {n} |").unwrap();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::Verdict;
    use crate::report::{Outcome, Report};

    fn outcome(id: &str, area: &str, verdict: Verdict) -> Outcome {
        Outcome {
            id: id.into(),
            area: area.into(),
            features: vec![],
            verdict,
        }
    }

    fn results() -> Results {
        let outcomes = vec![
            outcome("a", "built-ins/Array", Verdict::Pass),
            outcome("b", "built-ins/Array", Verdict::Fail("x".into())),
            outcome("c", "built-ins/Math", Verdict::Pass),
            outcome("d", "language/expressions", Verdict::NotRun("x".into())),
            outcome("e", "harness/assert-false.js", Verdict::Pass),
            outcome("f", "harness/compare-array.js", Verdict::Fail("x".into())),
        ];
        let report = Report::build(
            "ferrojs",
            Some("2e58f87d8350ad51224617a0".into()),
            &outcomes,
            3,
        );
        // Round trip through the JSON `test262 --out` writes.
        Results::parse(&serde_json::to_string_pretty(&report).unwrap()).unwrap()
    }

    #[test]
    fn reads_the_test262_report() {
        let r = results();
        assert_eq!(r.total.total(), 6);
        assert_eq!(r.skipped_files, 3);
        let tops = r.by_top();
        assert_eq!(
            tops["built-ins"],
            Counts {
                passed: 2,
                failed: 1,
                not_run: 0
            }
        );
        assert_eq!(tops["language"].not_run, 1);
        assert_eq!(tops["harness"].passed, 1);
        let dirs = r.by_directory();
        assert_eq!(dirs["harness"].total(), 2);
        assert!(!dirs.keys().any(|d| d.ends_with(".js")));
        assert!(Results::parse("{").is_err());
    }

    #[test]
    fn renders_directories_and_benchmarks() {
        let bench =
            Baseline::parse("# toolchain: rustc 1 / valgrind 2\nvalue/x 100\nvalue/y 7\n").unwrap();
        let md = render(&results(), &bench, "abc1234 (2026-11-20)");
        assert!(md.contains("- FerroJS revision: abc1234 (2026-11-20)"));
        assert!(md.contains("- Test262 suite commit: 2e58f87d8350\n"));
        assert!(
            md.contains("**3 of 6 runs pass (50.00%).** 2 fail, 1 were not run, 3 files skipped.")
        );
        assert!(md.contains("| built-ins | 2 | 3 | 66.67% |"));
        assert!(md.contains("| **all** | 3 | 6 | 50.00% |"));
        assert!(md.contains("### built-ins\n"));
        assert!(md.contains("| built-ins/Array | 1 | 2 | 50.00% |"));
        assert!(md.contains("| built-ins/Math | 1 | 1 | 100.00% |"));
        // A top-level directory's section lists only its own areas.
        let lang = md.split("### language").nth(1).unwrap();
        let lang = lang.split("###").next().unwrap();
        assert!(lang.contains("language/expressions"));
        assert!(!lang.contains("built-ins/"));
        assert!(md.contains("| harness | 1 | 2 | 50.00% |"));
        assert!(!md.contains("assert-false.js"));
        assert!(md.contains("toolchain: rustc 1 / valgrind 2"));
        assert!(md.contains("| value/x | 100 |"));
        // Same inputs, same page.
        assert_eq!(md, render(&results(), &bench, "abc1234 (2026-11-20)"));
    }

    #[test]
    fn no_benchmarks_yet() {
        let md = render(&results(), &Baseline::default(), "x");
        assert!(md.contains("No benchmarks yet."));
    }
}
