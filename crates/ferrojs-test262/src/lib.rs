//! Test262 conformance runner for FerroJS.
//!
//! Finds tests, expands each into the runs Test262 requires, hands them to an
//! [`engine::Engine`], judges the results by Test262's rules and reports pass
//! rates. A saved baseline of passing tests turns it into a regression gate:
//! any test that used to pass and now fails stops the build.

pub mod dashboard;
pub mod engine;
pub mod job;
pub mod judge;
pub mod metadata;
pub mod report;

use engine::Engine;
use job::{Harness, Selection};
use rayon::prelude::*;
use report::{Outcome, Report};
use std::path::Path;

/// Runs every selected test against `engine` and builds the report.
pub fn run_suite(
    test262_root: &Path,
    selection: &Selection,
    engine: &dyn Engine,
    test262_commit: Option<String>,
) -> std::io::Result<Report> {
    let harness = Harness::load(test262_root)?;
    let files = job::discover(test262_root, selection)?;

    let mut jobs = Vec::new();
    let mut skipped = 0;
    for rel in &files {
        match job::jobs_for(test262_root, &harness, rel, selection) {
            Ok(js) => jobs.extend(js),
            Err(_) => skipped += 1,
        }
    }

    let outcomes: Vec<Outcome> = jobs
        .par_iter()
        .map(|j| {
            let execution = engine.run(j);
            Outcome {
                id: j.id.clone(),
                area: j.area(),
                features: j.features.clone(),
                verdict: judge::judge(j, &execution),
            }
        })
        .collect();

    Ok(Report::build(
        engine.name(),
        test262_commit,
        &outcomes,
        skipped,
    ))
}
