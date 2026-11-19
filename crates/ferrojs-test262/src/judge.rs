//! Pass/fail rules from Test262's INTERPRETING.md, applied identically to
//! every engine.

use crate::engine::Execution;
use crate::job::Job;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail(String),
    /// The engine couldn't run the test. Never counted as passing.
    NotRun(String),
}

const ASYNC_COMPLETE: &str = "Test262:AsyncTestComplete";
const ASYNC_FAILURE: &str = "Test262:AsyncTestFailure:";

pub fn judge(job: &Job, execution: &Execution) -> Verdict {
    let (error, printed) = match execution {
        Execution::Completed { error, printed } => (error, printed),
        Execution::Unsupported(why) => return Verdict::NotRun(why.clone()),
        Execution::Timeout => return Verdict::Fail("timed out".into()),
        Execution::Crash(why) => return Verdict::Fail(format!("engine crashed: {why}")),
    };

    if let Some(expected) = &job.negative {
        return match error {
            None => Verdict::Fail(format!(
                "expected {} during {:?}, but nothing was thrown",
                expected.error_type, expected.phase
            )),
            Some(e) if e.name != expected.error_type => Verdict::Fail(format!(
                "expected {}, got {}: {}",
                expected.error_type, e.name, e.message
            )),
            Some(e) if e.phase.is_some_and(|p| p != expected.phase) => Verdict::Fail(format!(
                "expected {} during {:?}, but it was thrown during {:?}",
                expected.error_type,
                expected.phase,
                e.phase.expect("checked is_some")
            )),
            Some(_) => Verdict::Pass,
        };
    }

    if let Some(e) = error {
        return Verdict::Fail(format!("{}: {}", e.name, e.message));
    }

    if job.is_async {
        if let Some(failure) = printed.iter().find(|p| p.starts_with(ASYNC_FAILURE)) {
            return Verdict::Fail(failure[ASYNC_FAILURE.len()..].trim().to_owned());
        }
        if !printed.iter().any(|p| p == ASYNC_COMPLETE) {
            return Verdict::Fail("async test never reported completion".into());
        }
    }

    Verdict::Pass
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Thrown;
    use crate::job::Mode;
    use crate::metadata::{Negative, Phase};

    fn job(negative: Option<Negative>, is_async: bool) -> Job {
        Job {
            id: "t.js:default".into(),
            rel_path: "t.js".into(),
            path: "t.js".into(),
            mode: Mode::NonStrict,
            prelude: String::new(),
            body: String::new(),
            negative,
            is_async,
            features: Vec::new(),
        }
    }

    fn done(error: Option<Thrown>, printed: &[&str]) -> Execution {
        Execution::Completed {
            error,
            printed: printed.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn err(phase: Option<Phase>, name: &str) -> Option<Thrown> {
        Some(Thrown {
            phase,
            name: name.into(),
            message: "m".into(),
        })
    }

    #[test]
    fn plain_tests() {
        assert_eq!(judge(&job(None, false), &done(None, &[])), Verdict::Pass);
        assert!(matches!(
            judge(
                &job(None, false),
                &done(err(Some(Phase::Runtime), "Test262Error"), &[])
            ),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn negative_tests_need_matching_name_and_phase() {
        let neg = Some(Negative {
            phase: Phase::Parse,
            error_type: "SyntaxError".into(),
        });
        let j = job(neg, false);
        assert_eq!(
            judge(&j, &done(err(Some(Phase::Parse), "SyntaxError"), &[])),
            Verdict::Pass
        );
        assert_eq!(
            judge(&j, &done(err(None, "SyntaxError"), &[])),
            Verdict::Pass
        );
        assert!(matches!(
            judge(&j, &done(err(Some(Phase::Runtime), "SyntaxError"), &[])),
            Verdict::Fail(_)
        ));
        assert!(matches!(
            judge(&j, &done(err(Some(Phase::Parse), "TypeError"), &[])),
            Verdict::Fail(_)
        ));
        assert!(matches!(judge(&j, &done(None, &[])), Verdict::Fail(_)));
    }

    #[test]
    fn async_tests_must_report_completion() {
        let j = job(None, true);
        assert_eq!(judge(&j, &done(None, &[ASYNC_COMPLETE])), Verdict::Pass);
        assert!(matches!(judge(&j, &done(None, &[])), Verdict::Fail(_)));
        assert!(matches!(
            judge(
                &j,
                &done(
                    None,
                    &[
                        "Test262:AsyncTestFailure:Test262Error: boom",
                        ASYNC_COMPLETE
                    ]
                )
            ),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn engine_problems() {
        let j = job(None, false);
        assert!(matches!(
            judge(&j, &Execution::Unsupported("x".into())),
            Verdict::NotRun(_)
        ));
        assert!(matches!(judge(&j, &Execution::Timeout), Verdict::Fail(_)));
        assert!(matches!(
            judge(&j, &Execution::Crash("x".into())),
            Verdict::Fail(_)
        ));
    }
}
