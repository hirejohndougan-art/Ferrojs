//! Whole scripts and globals (task 1.6e, D-042) and strings (task 1.6f,
//! D-044): each script runs on the toy context, and the global `result`
//! (or the error it throws) must match Node.js running the same script in
//! a fresh context. Strings are compared as UTF-16 code units.

use ferrojs_bytecode::testing::{run_program, Outcome, ToyCtx, ToyFunction};
use ferrojs_bytecode::{verify, verify_handlers, ErrorKind};
use ferrojs_compiler::{compile_script_source, CompileError, CompiledFunction};
use ferrojs_value::{Kind, Value};

fn flatten(f: &CompiledFunction, cx: &mut ToyCtx, out: &mut Vec<ToyFunction>) -> usize {
    let limits = f.limits();
    verify(&f.code, &limits).expect("verifies");
    verify_handlers(&f.code, &f.handlers, &limits).expect("handlers verify");
    let index = out.len();
    out.push(ToyFunction::default());
    let functions = f.functions.iter().map(|g| flatten(g, cx, out)).collect();
    out[index] = ToyFunction {
        code: f.code.clone(),
        handlers: f.handlers.clone(),
        constants: f.constant_values(|s| cx.intern(s)),
        limits,
        params: f.params,
        functions,
    };
    index
}

fn show(cx: &ToyCtx, v: Value) -> String {
    if let Some(s) = cx.string_of(v) {
        let units: Vec<String> = s.encode_utf16().map(|u| format!("{u:x}")).collect();
        return format!("s:{}", units.join(","));
    }
    match v.kind() {
        Kind::Undefined => "undefined".into(),
        Kind::Null => "null".into(),
        Kind::Bool(b) => b.to_string(),
        _ => match v.as_number() {
            Some(d) if d.is_nan() => "NaN".into(),
            Some(d) => format!("n:{:016x}", d.to_bits()),
            None => "function".into(),
        },
    }
}

/// Runs each script in order in one context (like several `<script>`
/// tags), then reports `result`.
fn run_scripts(sources: &[&str]) -> String {
    let mut cx = ToyCtx::default();
    // One program table for all scripts: closures made by one script keep
    // indexing into it when another script calls them.
    let mut program = Vec::new();
    for src in sources {
        let f = compile_script_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        let entry = flatten(&f, &mut cx, &mut program);
        match run_program(&program, entry, &[], &mut cx, 10_000_000) {
            Outcome::Returned(_) => {}
            Outcome::Threw(v) => {
                return match cx.last_error {
                    Some(
                        ErrorKind::UninitializedBinding
                        | ErrorKind::UnresolvableReference
                        | ErrorKind::InvalidAssignmentTarget,
                    ) => "throw:ReferenceError".into(),
                    Some(
                        ErrorKind::ConstAssignment
                        | ErrorKind::NotCallable
                        | ErrorKind::NullishBase
                        | ErrorKind::NotAnObject
                        | ErrorKind::PropertyRefused
                        | ErrorKind::HostResult,
                    ) => "throw:TypeError".into(),
                    Some(ErrorKind::GlobalRedeclaration) => "throw:SyntaxError".into(),
                    Some(ErrorKind::StackOverflow) => "throw:RangeError".into(),
                    None => format!("throw:{}", show(&cx, v)),
                }
            }
            Outcome::StepLimit => return "step limit".into(),
        }
    }
    match cx.global("result") {
        Some(v) => show(&cx, v),
        None => "no result".into(),
    }
}

include!("common/script_cases.rs");

fn match_node(cases: &[&[&str]]) {
    let Some(expected) = node_results(cases) else {
        eprintln!("node is not on the PATH; skipping the Node comparison");
        return;
    };
    assert_eq!(expected.len(), cases.len());
    for (case, want) in cases.iter().zip(&expected) {
        assert_eq!(&run_scripts(case), want, "{case:?}");
    }
}

#[test]
fn scripts_match_node() {
    match_node(SCRIPT_CASES);
}

#[test]
fn strings_match_node() {
    match_node(STRING_CASES);
}

#[test]
fn default_parameters_match_node() {
    match_node(DEFAULT_CASES);
}

#[test]
fn properties_match_node() {
    match_node(PROPERTY_CASES);
}

#[test]
fn script_level_unsupported_things_are_refused() {
    for src in [
        "this;",
        "class C {}",
        "function f() { return this; }",
        "x => this",
    ] {
        assert!(
            matches!(
                compile_script_source(src),
                Err(CompileError::Unsupported { .. })
            ),
            "{src}"
        );
    }
}

#[test]
fn property_forms_not_supported_yet_are_refused() {
    for src in [
        "var o = { get x() { return 1; } };",
        "var o = { set x(v) {} };",
        "var p = {}; var o = { ...p };",
        "var o = { __proto__: null };",
        "var o = { '__proto__': null };",
        "var o = {}; o?.x;",
        "var o = {}; o?.f();",
        "var o = {}; delete o?.x;",
        "var x = 1; delete x;",
        "var o = {}; var r = o instanceof o;",
        "var a = [1, 2];",
        "var o = { [1n]: 1 };",
    ] {
        assert!(
            matches!(
                compile_script_source(src),
                Err(CompileError::Unsupported { .. })
            ),
            "{src}"
        );
    }
    // These are ordinary properties, not the prototype.
    for src in [
        "var __proto__ = 1; var o = { __proto__ };",
        "var o = { ['__proto__']: 1 };",
        "var o = { __proto__() {} };",
    ] {
        compile_script_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    }
}

#[test]
fn computed_proto_key_is_a_plain_property() {
    assert_eq!(
        run_scripts(&["var o = { ['__proto__']: 5 }; var result = o['__proto__'];"]),
        "n:4014000000000000"
    );
}

#[test]
fn inherited_object_prototype_names_fail_cleanly() {
    // No Object.prototype until 1.13: these throw `undefined` with no
    // error kind instead of giving Node's answer wrongly.
    for src in [
        "var o = {}; var result = o.toString;",
        "var o = {}; var result = 'hasOwnProperty' in o;",
        "var o = {}; o.__proto__ = { a: 1 };",
        "var o = {}; var result = o.__proto__;",
        "var o = {}; o['__proto__'] = 1;",
    ] {
        assert_eq!(run_scripts(&[src]), "throw:undefined", "{src}");
    }
}
