//! Whole scripts and globals (task 1.6e, D-042): each script runs on the
//! toy context, and the global `result` (or the error it throws) must
//! match Node.js running the same script in a fresh context.

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

fn show(v: Value) -> String {
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
                    Some(ErrorKind::UninitializedBinding | ErrorKind::UnresolvableReference) => {
                        "throw:ReferenceError".into()
                    }
                    Some(ErrorKind::ConstAssignment | ErrorKind::NotCallable) => {
                        "throw:TypeError".into()
                    }
                    Some(ErrorKind::GlobalRedeclaration) => "throw:SyntaxError".into(),
                    Some(ErrorKind::StackOverflow) => "throw:RangeError".into(),
                    None => format!("throw:{}", show(v)),
                }
            }
            Outcome::StepLimit => return "step limit".into(),
        }
    }
    match cx.global("result") {
        Some(v) => show(v),
        None => "no result".into(),
    }
}

const CASES: &[&[&str]] = &[
    &["var result = 1 + 2;"],
    &["result = 7;"],
    &["let a = 2; const b = 3; var result = a * b;"],
    &["function sq(x) { return x * x; } var result = sq(9);"],
    &["var result = f(); function f() { return 5; }"],
    &["var result; result = typeof_missing;"],
    &["'use strict'; var result = 1; undeclared = 2;"],
    &["var result = 1; undeclared = 2; result = undeclared + 1;"],
    &["var result = x; let x = 1;"],
    &["const c = 1; c = 2; var result = c;"],
    &["let n = 0; function inc() { n++; return n; } inc(); inc(); var result = inc() * 10 + n;"],
    &["var counter = 0; function bump() { counter += 2; } bump(); bump(); var result = counter;"],
    &["var result = 0; for (var i = 0; i < 5; i++) result += i; result = result * 100 + i;"],
    &["var result = 0; for (let i = 0; i < 5; i++) result += i;"],
    &["var result = 0; { let x = 4; result = x; } "],
    &["var result = 1; if (true) { var result = 2; }"],
    &["function f() { return g(); } function g() { return 42; } var result = f();"],
    &["function f() { 'use strict'; return this; } var result = f();"],
    &["var result = (function () { 'use strict'; return this === undefined; })();"],
    &["let result = 1;", "result = result + 1;"],
    &["var v = 1;", "let v = 2;"],
    &["let w = 1;", "var w = 2;"],
    &["let w = 1;", "let w = 2;"],
    &["implicit = 1;", "let implicit = 2; var result = implicit;"],
    &["const k = 5;", "var result = k * 2;"],
    &[
        "function f() { return later; }",
        "let later = 3;",
        "var result = f();",
    ],
    &[
        "function f() { return later; } var result = 0; try { f(); } catch (e) { result = 1; }",
        "let later = 3;",
    ],
    &["var result = 0; try { missing; } catch (e) { result = 9; }"],
    &["var result = 0; function f(a) { if (a) { return result = a; } } f(4);"],
    &["var result = (() => { let s = 0; for (let i = 1; i <= 10; i++) s += i; return s; })();"],
    &["var NaNs = NaN; var result = NaNs !== NaNs && undefined === void 0;"],
];

fn node_results() -> Option<Vec<String>> {
    let mut script = String::from(
        "const vm=require('vm');\
         function show(v){if(v===undefined)return'undefined';if(v===null)return'null';\
         if(typeof v==='boolean')return String(v);if(typeof v==='function')return'function';\
         if(Number.isNaN(v))return'NaN';\
         const d=new DataView(new ArrayBuffer(8));d.setFloat64(0,v);\
         return'n:'+d.getBigUint64(0).toString(16).padStart(16,'0');}\n\
         function run(srcs){const c=vm.createContext({});\
         for(const s of srcs){try{vm.runInContext(s,c);}catch(e){\
         return 'throw:'+(e&&typeof e==='object'&&'name' in e?e.name:show(e));}}\
         try{return show(vm.runInContext('result',c));}catch(e){return 'no result';}}\nconst out=[];\n",
    );
    for case in CASES {
        let json: Vec<String> = case.iter().map(|s| format!("{:?}", s)).collect();
        script.push_str(&format!("out.push(run([{}]));\n", json.join(",")));
    }
    script.push_str("console.log(out.join('\\n'));\n");
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        String::from_utf8(out.stdout)
            .ok()?
            .lines()
            .map(str::to_owned)
            .collect(),
    )
}

#[test]
fn scripts_match_node() {
    let Some(expected) = node_results() else {
        eprintln!("node is not on the PATH; skipping the Node comparison");
        return;
    };
    assert_eq!(expected.len(), CASES.len());
    for (case, want) in CASES.iter().zip(&expected) {
        assert_eq!(&run_scripts(case), want, "{case:?}");
    }
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
