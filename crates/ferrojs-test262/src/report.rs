//! Results: pass rates overall, by area and by feature, plus the regression
//! gate that compares a run against a saved baseline of passing tests.

use crate::judge::Verdict;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    pub passed: usize,
    pub failed: usize,
    pub not_run: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.not_run
    }

    /// Passing share of all runs, in percent.
    pub fn percent(&self) -> f64 {
        if self.total() == 0 {
            0.0
        } else {
            100.0 * self.passed as f64 / self.total() as f64
        }
    }

    /// Adds another set of counts to this one.
    pub fn merge(&mut self, other: &Counts) {
        self.passed += other.passed;
        self.failed += other.failed;
        self.not_run += other.not_run;
    }

    fn add(&mut self, verdict: &Verdict) {
        match verdict {
            Verdict::Pass => self.passed += 1,
            Verdict::Fail(_) => self.failed += 1,
            Verdict::NotRun(_) => self.not_run += 1,
        }
    }
}

/// One finished run of one test.
pub struct Outcome {
    pub id: String,
    pub area: String,
    pub features: Vec<String>,
    pub verdict: Verdict,
}

#[derive(Serialize)]
pub struct Failure {
    pub id: String,
    pub reason: String,
}

#[derive(Serialize)]
pub struct Report {
    pub engine: String,
    pub test262_commit: Option<String>,
    pub total: Counts,
    pub skipped_files: usize,
    pub by_area: BTreeMap<String, Counts>,
    pub by_feature: BTreeMap<String, Counts>,
    pub failures: Vec<Failure>,
    #[serde(skip)]
    pub passing: BTreeSet<String>,
    #[serde(skip)]
    pub ran: BTreeSet<String>,
}

impl Report {
    pub fn build(
        engine: &str,
        test262_commit: Option<String>,
        outcomes: &[Outcome],
        skipped_files: usize,
    ) -> Report {
        let mut report = Report {
            engine: engine.to_owned(),
            test262_commit,
            total: Counts::default(),
            skipped_files,
            by_area: BTreeMap::new(),
            by_feature: BTreeMap::new(),
            failures: Vec::new(),
            passing: BTreeSet::new(),
            ran: BTreeSet::new(),
        };
        for o in outcomes {
            report.total.add(&o.verdict);
            report
                .by_area
                .entry(o.area.clone())
                .or_default()
                .add(&o.verdict);
            for f in &o.features {
                report
                    .by_feature
                    .entry(f.clone())
                    .or_default()
                    .add(&o.verdict);
            }
            report.ran.insert(o.id.clone());
            match &o.verdict {
                Verdict::Pass => {
                    report.passing.insert(o.id.clone());
                }
                Verdict::Fail(reason) => report.failures.push(Failure {
                    id: o.id.clone(),
                    reason: reason.clone(),
                }),
                Verdict::NotRun(_) => {}
            }
        }
        report.failures.sort_by(|a, b| a.id.cmp(&b.id));
        report
    }

    /// Tests that passed in the baseline and were run this time but didn't
    /// pass. Tests not run this time (e.g. filtered out) are not regressions.
    pub fn regressions(&self, baseline: &BTreeSet<String>) -> Vec<String> {
        baseline
            .iter()
            .filter(|id| self.ran.contains(*id) && !self.passing.contains(*id))
            .cloned()
            .collect()
    }

    /// Tests passing now that the baseline didn't list.
    pub fn newly_passing(&self, baseline: &BTreeSet<String>) -> usize {
        self.passing
            .iter()
            .filter(|id| !baseline.contains(*id))
            .count()
    }

    /// Short text summary for the terminal and CI logs.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "Test262 results for {} (suite commit {})\n  total: {} runs, {} passed ({:.2}%), {} failed, {} not run; {} files skipped\n",
            self.engine,
            self.test262_commit.as_deref().unwrap_or("unknown"),
            self.total.total(),
            self.total.passed,
            self.total.percent(),
            self.total.failed,
            self.total.not_run,
            self.skipped_files,
        );
        let mut tops: BTreeMap<&str, Counts> = BTreeMap::new();
        for (area, c) in &self.by_area {
            let top = area.split('/').next().unwrap_or(area);
            tops.entry(top).or_default().merge(c);
        }
        for (top, c) in tops {
            s.push_str(&format!(
                "  {top:<12} {:>6} / {:<6} {:6.2}%\n",
                c.passed,
                c.total(),
                c.percent()
            ));
        }
        s
    }
}

pub fn parse_baseline(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

pub fn format_baseline(passing: &BTreeSet<String>) -> String {
    let mut out =
        String::from("# FerroJS Test262 baseline: every test run that passes. One id per line.\n");
    for id in passing {
        out.push_str(id);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, verdict: Verdict) -> Outcome {
        Outcome {
            id: id.into(),
            area: "built-ins/Array".into(),
            features: vec!["Symbol".into()],
            verdict,
        }
    }

    #[test]
    fn counts_and_regressions() {
        let outcomes = vec![
            outcome("a.js:default", Verdict::Pass),
            outcome("b.js:default", Verdict::Fail("x".into())),
            outcome("c.js:default", Verdict::NotRun("x".into())),
        ];
        let r = Report::build("e", None, &outcomes, 0);
        assert_eq!(
            r.total,
            Counts {
                passed: 1,
                failed: 1,
                not_run: 1
            }
        );
        assert_eq!(r.by_feature["Symbol"].total(), 3);

        let baseline =
            parse_baseline("# header\na.js:default\nb.js:default\nnot-run-this-time.js:default\n");
        assert_eq!(r.regressions(&baseline), ["b.js:default"]);
        assert_eq!(r.newly_passing(&parse_baseline("")), 1);
        assert_eq!(parse_baseline(&format_baseline(&r.passing)), r.passing);
    }
}
