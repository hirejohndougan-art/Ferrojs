//! The bytecode compiler, part 1a (D-033): every compiled function passes
//! the verifier, and running it on the toy context gives exactly what
//! Node.js gives for the same call.

use ferrojs_bytecode::testing::{
    run, run_program, run_with_handlers, Outcome, ToyCtx, ToyFunction,
};
use ferrojs_bytecode::{disassemble, verify, verify_handlers, ErrorKind};
use ferrojs_compiler::{compile_function_source, CompileError, CompiledFunction, Constant};
use ferrojs_value::{Kind, Value};

fn compile(src: &str) -> CompiledFunction {
    let f = compile_function_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    check_tree(src, &f);
    f
}

/// Every function in the tree passes the verifier.
fn check_tree(src: &str, f: &CompiledFunction) {
    let checked = verify(&f.code, &f.limits())
        .and_then(|()| verify_handlers(&f.code, &f.handlers, &f.limits()));
    if let Err(e) = checked {
        panic!(
            "{src}: compiled code fails verification: {e:?}\n{}\n{:?}",
            disassemble(&f.code).unwrap_or_default(),
            f.handlers
        );
    }
    for g in &f.functions {
        check_tree(src, g);
    }
}

/// The function tree as a flat toy program; the root is index 0.
fn flatten(f: &CompiledFunction, cx: &mut ToyCtx, out: &mut Vec<ToyFunction>) -> usize {
    let index = out.len();
    out.push(ToyFunction::default());
    let functions = f.functions.iter().map(|g| flatten(g, cx, out)).collect();
    out[index] = ToyFunction {
        code: f.code.clone(),
        handlers: f.handlers.clone(),
        constants: f.constant_values(|s| cx.intern(s)),
        limits: f.limits(),
        params: f.params,
        functions,
    };
    index
}

fn call(f: &CompiledFunction, args: &[f64]) -> Outcome {
    call_toy(f, args).0
}

fn call_toy(f: &CompiledFunction, args: &[f64]) -> (Outcome, ToyCtx) {
    let mut program = Vec::new();
    let mut cx = ToyCtx::default();
    flatten(f, &mut cx, &mut program);
    let args: Vec<Value> = args.iter().map(|&a| Value::number(a)).collect();
    let out = run_program(&program, 0, &args, &mut cx, 10_000_000);
    (out, cx)
}

fn show(v: Value) -> String {
    match v.kind() {
        Kind::Undefined => "undefined".into(),
        Kind::Null => "null".into(),
        Kind::Bool(b) => b.to_string(),
        _ => match v.as_number() {
            Some(d) if d.is_nan() => "NaN".into(),
            Some(d) => format!("n:{:016x}", d.to_bits()),
            None => format!("{v:?}"),
        },
    }
}

fn show_outcome((o, cx): (Outcome, ToyCtx)) -> String {
    match o {
        Outcome::Returned(v) => show(v),
        // The toy has no error objects; it records which error was thrown.
        Outcome::Threw(v) => match cx.last_error {
            Some(
                ErrorKind::UninitializedBinding
                | ErrorKind::UnresolvableReference
                | ErrorKind::InvalidAssignmentTarget,
            ) => "throw:ReferenceError".into(),
            Some(ErrorKind::GlobalRedeclaration) => "throw:SyntaxError".into(),
            Some(
                ErrorKind::ConstAssignment
                | ErrorKind::NotCallable
                | ErrorKind::NullishBase
                | ErrorKind::NotAnObject
                | ErrorKind::PropertyRefused
                | ErrorKind::HostResult,
            ) => "throw:TypeError".into(),
            Some(ErrorKind::StackOverflow) => "throw:RangeError".into(),
            None => format!("throw:{}", show(v)),
        },
        Outcome::StepLimit => "step limit".into(),
    }
}

include!("common/function_cases.rs");

fn node_results() -> Option<Vec<String>> {
    let mut script = String::from(
        "function show(v){if(v===undefined)return'undefined';if(v===null)return'null';\
         if(typeof v==='boolean')return String(v);if(Number.isNaN(v))return'NaN';\
         const d=new DataView(new ArrayBuffer(8));d.setFloat64(0,v);\
         return'n:'+d.getBigUint64(0).toString(16).padStart(16,'0');}\n\
         const out=[];\n",
    );
    for (src, calls) in CASES {
        for args in *calls {
            let args: Vec<String> = args
                .iter()
                .map(|a| {
                    if a.is_nan() {
                        "NaN".into()
                    } else if a.is_infinite() {
                        if *a > 0.0 {
                            "Infinity".into()
                        } else {
                            "-Infinity".into()
                        }
                    } else if *a == 0.0 && a.is_sign_negative() {
                        "-0".into()
                    } else {
                        format!("{a:e}")
                    }
                })
                .collect();
            script.push_str(&format!(
                "out.push((()=>{{{src}\ntry{{return show(f({}));}}catch(e){{return'throw:'+(e instanceof Error?e.name:show(e));}}}})());\n",
                args.join(",")
            ));
        }
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
fn compiled_functions_match_node() {
    let Some(expected) = node_results() else {
        eprintln!("node is not on the PATH; skipping the Node comparison");
        return;
    };
    let mut i = 0;
    let mut checked = 0;
    for (src, calls) in CASES {
        if calls.is_empty() {
            continue;
        }
        let f = compile(src);
        for args in *calls {
            let got = show_outcome(call_toy(&f, args));
            assert_eq!(got, expected[i], "{src} with {args:?}");
            i += 1;
            checked += 1;
        }
    }
    assert_eq!(i, expected.len());
    assert!(checked > 80, "only {checked} calls checked");
}

#[test]
fn hand_checked_results() {
    let f =
        compile("function sum(n) { let s = 0; for (let i = 1; i <= n; i++) s += i; return s; }");
    assert_eq!(f.name, "sum");
    assert_eq!(f.params, 1);
    assert_eq!(call(&f, &[100.0]), Outcome::Returned(Value::from_i32(5050)));
    let f = compile("function f() {}");
    assert_eq!(call(&f, &[]), Outcome::Returned(Value::UNDEFINED));
    let f = compile("function f(x) { 'use strict'; return x; }");
    assert!(f.strict);
    // `x` itself, plus the temporary of the implicit `return undefined`:
    // reading a parameter needs no temporary of its own.
    assert_eq!(f.registers, 2);
    let f = compile("function f(x) { return 0.5 + x + 0.5 + 1e100; }");
    assert_eq!(
        f.constants,
        vec![Constant::Number(0.5), Constant::Number(1e100)],
        "constants are shared"
    );
}

#[test]
fn termination_is_not_catchable() {
    let f =
        compile("function f() { for (;;) { try { while (true) {} } catch (e) {} finally {} } }");
    assert!(!f.handlers.is_empty());
    let mut cx = ToyCtx::new(&f.limits());
    cx.interrupt_after = Some(1000);
    let out = run_with_handlers(&f.code, &f.handlers, &mut cx, 1_000_000);
    assert_eq!(out, Outcome::Threw(Value::NULL));
}

#[test]
fn loops_check_for_interrupts() {
    let f = compile("function f() { while (true) {} }");
    let mut cx = ToyCtx::new(&f.limits());
    cx.interrupt_after = Some(1000);
    assert_eq!(
        run(&f.code, &mut cx, 1_000_000),
        Outcome::Threw(Value::NULL)
    );
}

fn unsupported(src: &str) -> String {
    match compile_function_source(src) {
        Err(CompileError::Unsupported { what, .. }) => what,
        other => panic!("expected {src:?} to be unsupported, got {other:?}"),
    }
}

#[test]
fn everything_else_is_unsupported_not_miscompiled() {
    for src in [
        "function f() { return '\\uD800'; }",
        "function f() { return `a\\uDC00${1}`; }",
        "function f() { return f`x`; }",
        "function f() { if (1) { function g() {} } }",
        "function f() { return { get '\\uD800'() { return 1; } }; }",
        "function f() { return new f(); }",
        "function f(a, g = () => a) { var a = 2; }",
        "function f(a = 1, g = function () { return a; }) { function a() {} }",
        "function f() { return arguments; }",
        "function f() { return () => typeof arguments; }",
        "function f() { arguments = 1; }",
        "function f(a = arguments) {}",
        "function f(...r) {}",
        "function f({ a }) {}",
        "function f() { let [a] = [1]; }",
        "function f() { return [1]; }",
        "function f() { return this; }",
        "function f(o) { return o?.p; }",
        "function f() { try {} catch ({ e }) {} }",
        "function f() { try {} catch (e) { var e = 1; } }",
        "function f() { for (var k in {}) ; }",
        "function* f() {}",
        "async function f() {}",
        "function f() { return 1n; }",
    ] {
        unsupported(src);
    }
    assert!(matches!(
        compile_function_source("function f( {"),
        Err(CompileError::Syntax(_))
    ));
}

#[test]
fn length_counts_parameters_before_the_first_default() {
    for (src, params, length) in [
        ("function f() {}", 0, 0),
        ("function f(a, b) {}", 2, 2),
        ("function f(a, b = 1, c) {}", 3, 1),
        ("function f(a = 1) {}", 1, 0),
    ] {
        let f = compile(src);
        assert_eq!((f.params, f.length), (params, length), "{src}");
    }
}

#[test]
fn proper_tail_calls_keep_the_stack_flat() {
    // 100,000 calls deep: a RangeError without tail calls (the toy stops
    // at 10,000 frames), fine with them. Node.js doesn't implement proper
    // tail calls, so this one is checked by hand.
    let src = "function f(n) { 'use strict'; function loop(k, acc) { return k === 0 ? acc : loop(k - 1, acc + 1); } return loop(n, 0); }";
    let f = compile(src);
    assert_eq!(
        call(&f, &[100_000.0]),
        Outcome::Returned(Value::from_i32(100_000))
    );
    // Sloppy code has no tail calls; neither does a call inside `try`.
    for src in [
        "function f(n) { function loop(k, acc) { return k === 0 ? acc : loop(k - 1, acc + 1); } return loop(n, 0); }",
        "function f(n) { 'use strict'; function loop(k) { return k === 0 ? 0 : 1 + loop(k - 1); } return loop(n); }",
    ] {
        let f = compile(src);
        let (out, cx) = call_toy(&f, &[100_000.0]);
        assert!(matches!(out, Outcome::Threw(_)), "{src}");
        assert_eq!(cx.last_error, Some(ErrorKind::StackOverflow), "{src}");
    }
    // A return inside `try` isn't in tail position: the deepest frame
    // overflows and its handler sees the RangeError.
    let f = compile("function f(n) { 'use strict'; function loop(k) { try { return k === 0 ? 0 : loop(k - 1); } catch (e) { return -1; } } return loop(n); }");
    assert_eq!(
        call(&f, &[100_000.0]),
        Outcome::Returned(Value::from_i32(-1))
    );
}

#[test]
fn nested_functions_are_compiled_into_the_tree() {
    let f = compile("function f(a) { function g() { return () => a; } return g()(); }");
    assert_eq!(f.functions.len(), 1);
    assert_eq!(f.functions[0].name, "g");
    assert_eq!(f.functions[0].functions.len(), 1);
    assert_eq!(call(&f, &[7.0]), Outcome::Returned(Value::from_i32(7)));
}
