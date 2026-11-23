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

pub mod call_target;
pub mod codegen;
pub mod early;

pub use codegen::{
    compile_function, compile_function_source, compile_script, compile_script_source, CompileError,
    CompiledFunction, Constant,
};

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
    /// Sloppy-mode calls used as assignment targets (D-045): the span ends
    /// of the member expressions they were rewritten to, sorted. Empty for
    /// almost every program.
    pub call_targets: Vec<u32>,
    rewrites: call_target::Rewrites,
}

impl Parsed<'_> {
    /// Maps an offset in the AST's spans back to the source text passed to
    /// [`parse`]. The two differ only when the program has
    /// [`call_targets`](Parsed::call_targets).
    pub fn original_offset(&self, offset: u32) -> u32 {
        self.rewrites.original(offset)
    }
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
    let parse_text = |text: &'a str| {
        Parser::new(allocator, text, goal.source_type())
            .with_options(options)
            .parse()
    };
    let mut ret = parse_text(source);
    let mut rewrites = call_target::Rewrites::default();
    if !is_clean(&ret) {
        // Sloppy scripts may assign to calls, which oxc rejects: rewrite
        // each such call (D-045, `call_target`).
        rewrites = rewrite_call_targets(source, goal, options, &ret)?;
        ret = parse_text(allocator.alloc_str(&rewrites.apply(source)));
        if !is_clean(&ret) {
            return Err(syntax_errors(&ret, &rewrites));
        }
    }

    let program: &'a Program<'a> = allocator.alloc(ret.program);
    let built = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(program);
    let mut errors: Vec<SyntaxError> = built
        .diagnostics
        .errors()
        .map(|d| rewrites_error(&rewrites, SyntaxError::from_oxc(d)))
        .collect();
    errors.extend(
        early::check(program, goal)
            .into_iter()
            .map(|e| rewrites_error(&rewrites, e)),
    );
    if !errors.is_empty() {
        return Err(errors);
    }
    let call_targets = if rewrites.is_empty() {
        Vec::new()
    } else {
        call_target::validate(program, built.semantic.scoping(), &rewrites).map_err(|e| vec![e])?
    };
    Ok(Parsed {
        program,
        semantic: built.semantic,
        call_targets,
        rewrites,
    })
}

fn is_clean(ret: &oxc_parser::ParserReturn<'_>) -> bool {
    !ret.fatal_error && ret.diagnostics.errors().next().is_none()
}

/// The errors of a failed parse of `rewrites` applied to the source, at
/// offsets in the source.
fn syntax_errors(
    ret: &oxc_parser::ParserReturn<'_>,
    rewrites: &call_target::Rewrites,
) -> Vec<SyntaxError> {
    let errors = ret
        .diagnostics
        .errors()
        .map(|d| rewrites_error(rewrites, SyntaxError::from_oxc(d)))
        .collect();
    non_empty(errors, "invalid syntax")
}

/// If `ret` failed only because of a call used as an assignment target,
/// records a site for it. Returns whether it did.
fn add_call_site(
    ret: &oxc_parser::ParserReturn<'_>,
    goal: Goal,
    rewrites: &mut call_target::Rewrites,
    source: &str,
) -> bool {
    let mut errors = ret.diagnostics.errors();
    let (Some(d), None) = (errors.next(), errors.next()) else {
        return false;
    };
    if goal != Goal::Script || !ret.fatal_error || d.message != call_target::INVALID_TARGET {
        return false;
    }
    match d.labels.first() {
        Some(label) => rewrites.add(label.offset() + label.len(), source),
        None => false,
    }
}

/// Rewrites every call assignment target in `source`, given the failed
/// parse `first` of the unchanged text. The rounds parse in a scratch
/// arena so failed attempts don't pile up in the caller's. Returns the
/// errors of the last attempt if it fails for another reason.
fn rewrite_call_targets(
    source: &str,
    goal: Goal,
    options: ParseOptions,
    first: &oxc_parser::ParserReturn<'_>,
) -> Result<call_target::Rewrites, Vec<SyntaxError>> {
    let mut rewrites = call_target::Rewrites::default();
    if !add_call_site(first, goal, &mut rewrites, source) {
        return Err(syntax_errors(first, &rewrites));
    }
    let mut scratch = Allocator::default();
    loop {
        let text = rewrites.apply(source);
        {
            let ret = Parser::new(&scratch, &text, goal.source_type())
                .with_options(options)
                .parse();
            if is_clean(&ret) {
                return Ok(rewrites);
            }
            if !add_call_site(&ret, goal, &mut rewrites, source) {
                return Err(syntax_errors(&ret, &rewrites));
            }
        }
        scratch.reset();
    }
}

/// Maps a syntax error found in rewritten text back to the source.
fn rewrites_error(rewrites: &call_target::Rewrites, mut e: SyntaxError) -> SyntaxError {
    e.offset = e.offset.map(|o| rewrites.original(o));
    e
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
