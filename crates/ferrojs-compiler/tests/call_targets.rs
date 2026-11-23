//! Calls as assignment targets in sloppy code (task 1.6g, D-045):
//! `f() = 1` and friends parse in sloppy scripts, call `f`, then throw a
//! `ReferenceError` before anything else runs. Everywhere else they stay
//! `SyntaxError`s. Run results are checked against Node.js when it is on
//! the PATH, and against the expected strings below always.

use ferrojs_bytecode::testing::{run_program, Outcome, ToyCtx, ToyFunction};
use ferrojs_bytecode::{verify, verify_handlers, ErrorKind};
use ferrojs_compiler::{check, compile_script_source, parse, Allocator, CompiledFunction, Goal};
use ferrojs_value::{Kind, Value};

fn ok(src: &str) {
    if let Err(e) = check(src, Goal::Script) {
        panic!("expected {src:?} to parse, got {e}");
    }
}

fn err(src: &str, goal: Goal) {
    assert!(
        check(src, goal).is_err(),
        "expected {src:?} to be rejected as {goal:?}"
    );
}

#[test]
fn sloppy_call_targets_parse() {
    for src in [
        "f() = 1;",
        "f() += 1;",
        "f() **= 2;",
        "f() >>>= 1;",
        "f()++;",
        "f()--;",
        "++f();",
        "--f();",
        "for (f() in o);",
        "for (f() of o);",
        "async() = 1;",
        "(f()) = 1;",
        "((f())) += 1;",
        "a.b() = 1;",
        "a[0](1, 2)() = 1;",
        "f(g() = 1) = 2;",
        "x = f() = 1;",
        "f()\n= 1;",
        "f(...a) = 1;",
        "function t() { f() = 1; } function u() { 'use strict'; }",
        "() => { f()++; };",
        "if (0) f() = 1; else ++g();",
        "({ m() { super.x() = 1; } });",
        "f() = 1; 'use strict';",
        "var o = { f() {} }; o.f() = 1; for (f() in o) { g()--; }",
    ] {
        ok(src);
    }
}

#[test]
fn other_call_targets_stay_syntax_errors() {
    for src in [
        // Strict code.
        "'use strict'; f() = 1;",
        "'use strict'; f()++;",
        "'use strict'; for (f() in o);",
        "function g() { 'use strict'; f() = 1; }",
        "function g() { 'use strict'; return () => ++f(); }",
        "class C { m() { f() = 1; } }",
        "class C { static { f()++; } }",
        // Logical assignment needs a simple target.
        "f() &&= 1;",
        "f() ||= 1;",
        "f() ??= 1;",
        // Destructuring targets need a simple target.
        "[f()] = [1];",
        "[...f()] = [];",
        "({ a: f() } = {});",
        "({ a: f() = 1 } = {});",
        "for ([f()] of x);",
        // Not plain calls.
        "new f() = 1;",
        "new f++;",
        "f?.() = 1;",
        "a?.b() = 1;",
        "f`x` = 1;",
        "import(x) = 1;",
        "this = 1;",
        "1 = 1;",
        "a + b() = 1;",
        "(f(), g()) = 1;",
        "(a, f()) = 1;",
        "f() => 1 = 1;",
        "!f() = 1;",
        "f()() + 1 = 2;",
    ] {
        err(src, Goal::Script);
    }
    for src in ["f() = 1;", "f()++;", "for (f() of o);"] {
        err(src, Goal::Module);
    }
}

#[test]
fn offsets_are_in_the_original_source() {
    let allocator = Allocator::default();
    // A later error after a rewritten site keeps its own offset.
    let src = "f() = 1; g()++;\nlet a; let a;";
    let errors = match parse(&allocator, src, Goal::Script) {
        Ok(_) => panic!("redeclaration accepted"),
        Err(e) => e,
    };
    // oxc labels the first declaration; unmapped it would be 6 bytes later.
    let want = src.find("a;").unwrap() as u32;
    assert!(
        errors.iter().any(|e| e.offset == Some(want)),
        "{errors:?}, want an error at {want}"
    );
    // A site in strict code is reported at its call.
    let src = "x = 1; f() = 1; function s() { 'use strict'; h() = 2; }";
    let e = check(src, Goal::Script).unwrap_err();
    assert_eq!(e.offset, Some(src.find("h()").unwrap() as u32), "{e}");
    // Parsed spans are rewritten offsets; `original_offset` maps them back.
    let src = "f() = 1; g() = 2; var z;";
    let parsed = parse(&allocator, src, Goal::Script).unwrap();
    assert_eq!(parsed.call_targets.len(), 2);
    let last = parsed.program.body.last().unwrap();
    let start = oxc_span::GetSpan::span(last).start;
    assert_eq!(
        parsed.original_offset(start),
        src.find("var").unwrap() as u32
    );
    // No sites: no rewrite.
    let parsed = parse(&allocator, "x = 1;", Goal::Script).unwrap();
    assert!(parsed.call_targets.is_empty());
    assert_eq!(parsed.original_offset(4), 4);
}

#[test]
fn unsupported_offsets_are_in_the_original_source() {
    // Property assignment isn't compiled yet; its offset is reported in
    // the caller's source, past a rewritten site.
    let src = "f() = 1; o.p = 2; function f() {} var o;";
    match compile_script_source(src) {
        Err(ferrojs_compiler::CompileError::Unsupported { offset, .. }) => {
            assert_eq!(offset, src.find("o.p").unwrap() as u32);
        }
        other => panic!("{other:?}"),
    }
}

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

fn show(v: Value) -> String {
    match v.kind() {
        Kind::Undefined => "undefined".into(),
        Kind::Null => "null".into(),
        Kind::Bool(b) => b.to_string(),
        _ => match v.as_number() {
            Some(d) => format!("{d}"),
            None => "other".into(),
        },
    }
}

/// Runs `src` as a script and reports the global `result`, or the error
/// it throws.
fn run(src: &str) -> String {
    let mut cx = ToyCtx::default();
    let f = compile_script_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    let mut program = Vec::new();
    let entry = flatten(&f, &mut cx, &mut program);
    match run_program(&program, entry, &[], &mut cx, 1_000_000) {
        Outcome::Returned(_) => cx.global("result").map_or("no result".into(), show),
        Outcome::Threw(v) => match cx.last_error {
            Some(
                ErrorKind::UninitializedBinding
                | ErrorKind::UnresolvableReference
                | ErrorKind::InvalidAssignmentTarget,
            ) => "throw:ReferenceError".into(),
            Some(ErrorKind::ConstAssignment | ErrorKind::NotCallable) => "throw:TypeError".into(),
            Some(ErrorKind::GlobalRedeclaration) => "throw:SyntaxError".into(),
            Some(ErrorKind::StackOverflow) => "throw:RangeError".into(),
            None => format!("throw:{}", show(v)),
        },
        Outcome::StepLimit => "step limit".into(),
    }
}

/// `log` records the order calls happen in: f appends 1, g 2, k 3.
const PRELUDE: &str = "var log = 0;\
    function f() { log = log * 10 + 1; return 5; }\
    function g() { log = log * 10 + 2; return 6; }\
    function k() { log = log * 10 + 3; return f; }\n";

/// (script after the prelude, expected result)
const CASES: &[(&str, &str)] = &[
    ("f() = g();", "throw:ReferenceError"),
    ("f() += g();", "throw:ReferenceError"),
    ("f()++;", "throw:ReferenceError"),
    ("--f();", "throw:ReferenceError"),
    ("var result; try { f() = g(); } catch (e) { result = log; }", "1"),
    ("var result; try { f() *= g(); } catch (e) { result = log; }", "1"),
    ("var result; try { f()--; } catch (e) { result = log; }", "1"),
    ("var result; try { ++f(); } catch (e) { result = log; }", "1"),
    ("var result; try { k()() = g(); } catch (e) { result = log; }", "31"),
    ("var result; try { (f()) = g(); } catch (e) { result = log; }", "1"),
    ("var result; try { f(g()) = k(); } catch (e) { result = log; }", "21"),
    ("var result; try { g(f() = 1); } catch (e) { result = log; }", "1"),
    ("var result = 0; try { result = f() = g(); } catch (e) {} result = result + log;", "1"),
    ("var result = 7; if (log) f() = 1;", "7"),
    ("var result = 7; function never() { f()++; } result = result + log;", "7"),
    ("function t() { g(); f() = 1; } var result; try { t(); } catch (e) { result = log; }", "21"),
    ("function async() { log = 9; } var result; try { async() = 1; } catch (e) { result = log; }", "9"),
    ("const c = 1; function boom() { c = 2; } boom() = g();", "throw:TypeError"),
    ("function boom() { return missing; } boom() = g();", "throw:ReferenceError"),
    ("var result; try { f()\n= g(); } catch (e) { result = log; }", "1"),
    (
        "var result = 0; for (var i = 0; i < 3; i++) { try { f()++; } catch (e) { result++; } } result = result * 1000 + log;",
        "3111",
    ),
];

fn node_results() -> Option<Vec<String>> {
    let mut script = String::from(
        "const vm=require('vm');\
         function show(v){if(v===undefined)return'undefined';if(v===null)return'null';\
         if(typeof v==='boolean')return String(v);if(typeof v==='number')return String(v);return'other';}\n\
         function run(s){const c=vm.createContext({});\
         try{vm.runInContext(s,c);}catch(e){\
         return 'throw:'+(e&&typeof e==='object'&&'name' in e?e.name:show(e));}\
         try{return show(vm.runInContext('result',c));}catch(e){return 'no result';}}\nconst out=[];\n",
    );
    for (src, _) in CASES {
        script.push_str(&format!(
            "out.push(run({:?}));\n",
            format!("{PRELUDE}{src}")
        ));
    }
    script.push_str("console.log(out.join('\\n'));\n");
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(String::from)
            .collect(),
    )
}

#[test]
fn calls_run_then_throw_like_node() {
    let node = node_results();
    if node.is_none() {
        eprintln!("node not found; checking against the expected results only");
    }
    for (i, (src, want)) in CASES.iter().enumerate() {
        let full = format!("{PRELUDE}{src}");
        if let Some(node) = &node {
            assert_eq!(
                node[i], *want,
                "Node.js disagrees with the expectation for {src}"
            );
        }
        assert_eq!(run(&full), *want, "{src}");
    }
}

#[test]
fn site_count_is_capped() {
    // Each site costs a parse of the whole text, so there is a limit
    // (D-045); past it the program is refused rather than parsed slowly.
    ok(&"f() = 1;\n".repeat(256));
    err(&"f() = 1;\n".repeat(257), Goal::Script);
}
