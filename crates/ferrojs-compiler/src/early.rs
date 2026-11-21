//! Our own early-error pass (D-018, D-032): the early errors `oxc_parser`
//! and `oxc_semantic` miss.
//!
//! At the pinned Test262 commit and oxc 0.158, oxc reports every early
//! error Test262's negative syntax tests check (all negative parse-phase
//! runs pass), so this pass has no rules yet. It runs on every parse, so a
//! rule added here when a Test262 update or a fuzz finding shows a gap
//! applies everywhere. Each rule lands with a Test262 test or a unit test
//! in `tests/front_end.rs` that fails without it.

use crate::{Goal, SyntaxError};
use oxc_ast::ast::Program;

/// Returns the early errors in `program` that oxc does not report.
pub fn check(_program: &Program<'_>, _goal: Goal) -> Vec<SyntaxError> {
    Vec::new()
}
