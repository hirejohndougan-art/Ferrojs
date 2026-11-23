//! Call expressions as assignment targets in sloppy code (D-045).
//!
//! ECMA-262 gives `CallExpression` the assignment target type *web-compat*
//! in non-strict code: `f() = 1`, `f() += 1`, `f()++`, `--f()`,
//! `for (f() in o)` and `for (f() of o)` are valid scripts that call `f`
//! and then throw a `ReferenceError` (13.15.2, 13.4, 14.7.5). oxc 0.158
//! rejects them with a fatal "Cannot assign to this expression" error, and
//! its AST can't represent such a target.
//!
//! The front end works around it by rewriting the source: after the call
//! the error points at, it inserts [`MARKER`] (`[0]`), which turns the call
//! into a member expression oxc accepts, and parses again. Each insertion
//! is a *site*. Once the rewritten text parses, [`validate`] checks every
//! site in the AST: the member's object must be an ordinary call (not
//! `super()`, not optional), the member must be the whole target of `=`, an
//! arithmetic compound assignment, an update, or a `for`-`in`/`of` head,
//! and the code must be sloppy. Anything else is reported as the original
//! `SyntaxError`. The bytecode compiler evaluates the call of a site and
//! throws the `ReferenceError` instead of storing.
//!
//! Offsets in the AST are offsets in the rewritten text; [`Rewrites`] maps
//! them back to the caller's source.

use std::collections::HashSet;

use oxc_ast::ast::{
    AssignmentExpression, AssignmentTarget, ComputedMemberExpression, Expression, ForInStatement,
    ForOfStatement, ForStatementLeft, Program, SimpleAssignmentTarget, UpdateExpression,
};
use oxc_ast_visit::{walk, Visit};
use oxc_semantic::{ScopeFlags, ScopeId, Scoping};

use crate::SyntaxError;

/// The text inserted after a call used as an assignment target.
pub const MARKER: &str = "[0]";
const MARKER_LEN: u32 = MARKER.len() as u32;

/// The message oxc reports for an invalid assignment target.
pub(crate) const INVALID_TARGET: &str = "Cannot assign to this expression";

/// Upper bound on sites in one source text: each round re-parses the whole
/// text, so the cost is sites × length. Past it the program is refused with
/// oxc's `SyntaxError` (a known deviation, D-045).
const MAX_SITES: usize = 256;

/// The insertions made into a source text, in original offsets.
#[derive(Clone, Debug, Default)]
pub struct Rewrites {
    /// Original offsets (the end of each call), sorted, distinct.
    at: Vec<u32>,
}

impl Rewrites {
    /// No insertions yet.
    pub fn is_empty(&self) -> bool {
        self.at.is_empty()
    }

    /// The original source with every marker inserted.
    pub fn apply(&self, source: &str) -> String {
        let mut out = String::with_capacity(source.len() + self.at.len() * MARKER.len());
        let mut from = 0;
        for &p in &self.at {
            out.push_str(&source[from..p as usize]);
            out.push_str(MARKER);
            from = p as usize;
        }
        out.push_str(&source[from..]);
        out
    }

    /// Where each marker ends in the rewritten text: the span end of the
    /// member expression a site becomes. Sorted.
    pub fn marker_ends(&self) -> Vec<u32> {
        (1..=self.at.len() as u32)
            .zip(&self.at)
            .map(|(n, &p)| p + n * MARKER_LEN)
            .collect()
    }

    /// Maps an offset in the rewritten text back to the original source.
    /// An offset inside a marker maps to the end of its call.
    pub fn original(&self, rewritten: u32) -> u32 {
        let mut shift = 0;
        for &p in &self.at {
            let start = p + shift;
            if rewritten <= start {
                break;
            }
            if rewritten < start + MARKER_LEN {
                return p;
            }
            shift += MARKER_LEN;
        }
        rewritten - shift
    }

    /// Records a marker after the call that ends at `rewritten_end` in the
    /// current rewritten text. Returns false if it can't be a new site: the
    /// expression ends right after a marker (the rewrite didn't help, as in
    /// `a + b() = 1`), the site exists already, or there are too many.
    pub fn add(&mut self, rewritten_end: u32, source: &str) -> bool {
        if self.at.len() >= MAX_SITES || self.marker_ends().contains(&rewritten_end) {
            return false;
        }
        let p = self.original(rewritten_end);
        // A call ends with the `)` of its arguments.
        if p == 0 || source.as_bytes().get(p as usize - 1) != Some(&b')') {
            return false;
        }
        match self.at.binary_search(&p) {
            Ok(_) => false,
            Err(i) => {
                self.at.insert(i, p);
                true
            }
        }
    }
}

/// Checks every site of a parsed rewritten program (see the module docs).
/// Returns the sites' member expressions by span end (the marker ends), or
/// the error for the first invalid one, at the original offset of its call.
pub fn validate(
    program: &Program<'_>,
    scoping: &Scoping,
    rewrites: &Rewrites,
) -> Result<Vec<u32>, SyntaxError> {
    let ends = rewrites.marker_ends();
    let mut v = Validator {
        scoping,
        ends: &ends,
        scopes: Vec::new(),
        accepted: HashSet::new(),
        bad: None,
    };
    v.visit_program(program);
    if let Some(start) = v.bad {
        return Err(SyntaxError {
            message: INVALID_TARGET.into(),
            offset: Some(rewrites.original(start)),
        });
    }
    if v.accepted.len() != ends.len() {
        // Every marker is a member expression, so every site is visited;
        // this is a safety net.
        return Err(SyntaxError {
            message: INVALID_TARGET.into(),
            offset: ends.first().map(|&e| rewrites.original(e)),
        });
    }
    Ok(ends)
}

/// Whether `target`, compiled as an assignment target, is a site: a call
/// whose result the program assigns to. Returns the call.
pub fn site_call<'b, 'a>(
    target: &'b SimpleAssignmentTarget<'a>,
    sites: &[u32],
) -> Option<&'b Expression<'a>> {
    match target {
        SimpleAssignmentTarget::ComputedMemberExpression(m)
            if sites.binary_search(&m.span.end).is_ok() =>
        {
            Some(&m.object)
        }
        _ => None,
    }
}

struct Validator<'v> {
    scoping: &'v Scoping,
    ends: &'v [u32],
    scopes: Vec<ScopeId>,
    /// Sites found in an allowed position.
    accepted: HashSet<u32>,
    /// The start of the first site found anywhere else (rewritten offset).
    bad: Option<u32>,
}

impl Validator<'_> {
    fn is_site(&self, m: &ComputedMemberExpression<'_>) -> bool {
        self.ends.binary_search(&m.span.end).is_ok()
    }

    fn strict(&self) -> bool {
        self.scopes
            .last()
            .is_some_and(|&s| self.scoping.scope_flags(s).is_strict_mode())
    }

    /// A site in an allowed position: accept it if the rest holds.
    fn target(&mut self, t: &SimpleAssignmentTarget<'_>) {
        let SimpleAssignmentTarget::ComputedMemberExpression(m) = t else {
            return;
        };
        if !self.is_site(m) {
            return;
        }
        let plain_call = matches!(
            &m.object,
            Expression::CallExpression(c) if !c.optional && !c.callee.is_super()
        );
        if plain_call && !self.strict() {
            self.accepted.insert(m.span.end);
        }
    }

    fn assignment_target(&mut self, t: &AssignmentTarget<'_>) {
        if let Some(s) = t.as_simple_assignment_target() {
            self.target(s);
        }
    }
}

impl<'a> Visit<'a> for Validator<'_> {
    fn enter_scope(&mut self, _flags: ScopeFlags, scope_id: &std::cell::Cell<Option<ScopeId>>) {
        let id = scope_id.get().or(self.scopes.last().copied());
        if let Some(id) = id {
            self.scopes.push(id);
        }
    }

    fn leave_scope(&mut self) {
        self.scopes.pop();
    }

    fn visit_assignment_expression(&mut self, it: &AssignmentExpression<'a>) {
        // Logical assignment needs a simple target (13.15.1).
        if !it.operator.is_logical() {
            self.assignment_target(&it.left);
        }
        walk::walk_assignment_expression(self, it);
    }

    fn visit_update_expression(&mut self, it: &UpdateExpression<'a>) {
        self.target(&it.argument);
        walk::walk_update_expression(self, it);
    }

    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        if let Some(t) = for_target(&it.left) {
            self.assignment_target(t);
        }
        walk::walk_for_in_statement(self, it);
    }

    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        if let Some(t) = for_target(&it.left) {
            self.assignment_target(t);
        }
        walk::walk_for_of_statement(self, it);
    }

    fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
        if self.is_site(it) && !self.accepted.contains(&it.span.end) && self.bad.is_none() {
            self.bad = Some(it.span.start);
        }
        walk::walk_computed_member_expression(self, it);
    }
}

fn for_target<'b, 'a>(left: &'b ForStatementLeft<'a>) -> Option<&'b AssignmentTarget<'a>> {
    match left {
        ForStatementLeft::VariableDeclaration(_) => None,
        other => Some(other.to_assignment_target()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_and_map_offsets() {
        let src = "f() = g(); h()++";
        let mut r = Rewrites::default();
        assert!(r.add(3, src));
        // `f()[0] = g(); h()++`: h() now ends at 17.
        assert!(r.add(17, src));
        let text = r.apply(src);
        assert_eq!(text, "f()[0] = g(); h()[0]++");
        assert_eq!(r.marker_ends(), vec![6, 20]);
        assert_eq!(r.original(0), 0);
        assert_eq!(r.original(3), 3);
        assert_eq!(r.original(4), 3);
        assert_eq!(r.original(6), 3);
        assert_eq!(r.original(7), 4);
        assert_eq!(r.original(17), 14);
        assert_eq!(r.original(18), 14);
        assert_eq!(r.original(20), 14);
        assert_eq!(r.original(22), 16);
    }

    #[test]
    fn add_refuses_non_sites() {
        let src = "a + b() = 1";
        let mut r = Rewrites::default();
        assert!(r.add(7, src));
        // `a + b()[0] = 1` fails again on `a + b()[0]`, ending at the marker.
        assert!(!r.add(10, src));
        // The same call twice, and something that doesn't end with `)`.
        assert!(!r.add(7, src));
        let mut r = Rewrites::default();
        assert!(!r.add(1, "x = 1"));
        assert!(!r.add(0, ")"));
    }
}
