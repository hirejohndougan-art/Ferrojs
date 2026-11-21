//! Front end of FerroJS: parsing and early errors (D-018), and from task
//! 1.6 on, bytecode generation.
//!
//! Source text is parsed by `oxc_parser`, then `oxc_semantic` builds scopes
//! and symbols and reports the early errors it knows about. Our own pass
//! ([`early`]) runs last and covers the early errors oxc misses, found by
//! running Test262's negative syntax tests. The compiler reads the oxc AST
//! directly; there is no AST of our own.
//!
//! ```
//! use ferrojs_compiler::{check, Goal};
//!
//! assert!(check("let x = 1; x + 2;", Goal::Script).is_ok());
//! let err = check("let x; let x;", Goal::Script).unwrap_err();
//! assert!(err.message.contains("x"));
//! ```

pub mod codegen;
pub mod early;

pub use codegen::{compile_function, compile_function_source, CompileError, CompiledFunction};

/// The arena the AST is allocated in; re-exported so callers need no oxc
/// dependency of their own.
pub use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::{Semantic, SemanticBuilder};
use oxc_span::SourceType;

/// The ECMAScript goal symbol a source text is parsed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    /// A classic script (sloppy unless it starts with `"use strict"`).
    Script,
    /// A module: always strict, `import`/`export` and top-level `await`.
    Module,
}

impl Goal {
    fn source_type(self) -> SourceType {
        match self {
            Goal::Script => SourceType::script(),
            Goal::Module => SourceType::mjs(),
        }
    }
}

/// An early error: the source text is not a valid program for its goal. In
/// JavaScript terms, a `SyntaxError` thrown before any code runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    /// Byte offset of the start of the offending text, if known.
    pub offset: Option<u32>,
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.offset {
            Some(o) => write!(f, "{} (at byte {o})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for SyntaxError {}

impl SyntaxError {
    fn from_oxc(d: &oxc_diagnostics::OxcDiagnostic) -> SyntaxError {
        let offset = d.labels.first().map(|l| l.offset());
        SyntaxError {
            message: d.message.to_string(),
            offset,
        }
    }
}

/// A parsed, early-error-free program with its scope analysis. The bytecode
/// compiler (task 1.6) consumes this.
pub struct Parsed<'a> {
    pub program: &'a Program<'a>,
    pub semantic: Semantic<'a>,
}

/// Parses `source` with the given goal and runs every early-error check.
///
/// The AST lives in `allocator`. On failure, returns every syntax error
/// found, in the order the passes found them (never empty).
pub fn parse<'a>(
    allocator: &'a Allocator,
    source: &'a str,
    goal: Goal,
) -> Result<Parsed<'a>, Vec<SyntaxError>> {
    let options = ParseOptions {
        // Regular expression literals have their own early errors.
        parse_regular_expression: true,
        ..ParseOptions::default()
    };
    let ret = Parser::new(allocator, source, goal.source_type())
        .with_options(options)
        .parse();
    let errors: Vec<SyntaxError> = ret
        .diagnostics
        .errors()
        .map(SyntaxError::from_oxc)
        .collect();
    if ret.fatal_error || !errors.is_empty() {
        return Err(non_empty(errors, "invalid syntax"));
    }

    let program: &'a Program<'a> = allocator.alloc(ret.program);
    let built = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(program);
    let mut errors: Vec<SyntaxError> = built
        .diagnostics
        .errors()
        .map(SyntaxError::from_oxc)
        .collect();
    errors.extend(early::check(program, goal));
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Parsed {
        program,
        semantic: built.semantic,
    })
}

/// Checks `source` for syntax and early errors, discarding the AST. Returns
/// the first error.
pub fn check(source: &str, goal: Goal) -> Result<(), SyntaxError> {
    let allocator = Allocator::default();
    match parse(&allocator, source, goal) {
        Ok(_) => Ok(()),
        Err(mut errors) => Err(errors.swap_remove(0)),
    }
}

fn non_empty(mut errors: Vec<SyntaxError>, fallback: &str) -> Vec<SyntaxError> {
    if errors.is_empty() {
        errors.push(SyntaxError {
            message: fallback.into(),
            offset: None,
        });
    }
    errors
}
