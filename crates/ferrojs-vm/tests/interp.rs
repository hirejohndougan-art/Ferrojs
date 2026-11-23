//! The interpreter (task 1.7b, D-047): every compiled function of the
//! compiler's Node.js-checked cases gives the same result on the real
//! interpreter as on the toy context, with the heap verifier on, and again
//! under GC stress. Plus what only the interpreter has: the frame stack,
//! interrupts, stack overflow and closures held only by the heap.

use std::sync::atomic::Ordering;

use ferrojs_bytecode::testing::{run_program, Outcome as ToyOutcome, ToyCtx, ToyFunction};
use ferrojs_bytecode::ErrorKind;
use ferrojs_compiler::{compile_function_source, CompiledFunction};
use ferrojs_core::gc::CxOptions;
use ferrojs_core::heap::HeapOptions;
use ferrojs_value::{Kind, Value};
use ferrojs_vm::interp::Outcome;
use ferrojs_vm::{string, Vm, VmOptions};

include!("../../ferrojs-compiler/tests/common/function_cases.rs");

fn vm(stress: bool) -> Vm {
    Vm::new(VmOptions {
        heap_size: 64 * ferrojs_core::cage::CHUNK_SIZE as u64,
        heap: HeapOptions {
            poison: true,
            verify: true,
        },
        cx: CxOptions { gc_stress: stress },
        // 64k slots: deep enough for the cases, small enough to overflow
        // quickly.
        stack_slots: 1 << 16,
    })
    .unwrap()
}

fn compile(src: &str) -> CompiledFunction {
    compile_function_source(src).unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn show(v: Value) -> String {
    match v.kind() {
        Kind::Undefined => "undefined".into(),
        Kind::Null => "null".into(),
        Kind::Bool(b) => b.to_string(),
        Kind::Object(_) => "object".into(),
        Kind::String(_) => "string".into(),
        _ => match v.as_number() {
            Some(d) if d.is_nan() => "NaN".into(),
            Some(d) => format!("n:{:016x}", d.to_bits()),
            None => format!("{v:?}"),
        },
    }
}

fn show_error(v: Value, e: Option<ErrorKind>) -> String {
    match e {
        Some(e) => format!("throw:{e:?}"),
        None => format!("throw:{}", show(v)),
    }
}

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

fn on_toy(f: &CompiledFunction, args: &[Value]) -> String {
    let mut program = Vec::new();
    let mut cx = ToyCtx::default();
    flatten(f, &mut cx, &mut program);
    match run_program(&program, 0, args, &mut cx, 50_000_000) {
        ToyOutcome::Returned(v) => show(v),
        ToyOutcome::Threw(v) => show_error(v, cx.last_error),
        ToyOutcome::StepLimit => "step limit".into(),
    }
}

fn on_vm(vm: &mut Vm, f: &CompiledFunction, args: &[Value]) -> String {
    let base = vm.stack_top();
    let out = match vm.run_function(f, args).expect("loads") {
        Outcome::Returned(v) => show(v),
        Outcome::Threw(v, e) => show_error(v, e),
        other => format!("{other:?}"),
    };
    assert_eq!(vm.stack_top(), base, "the stack unwinds after a run");
    out
}

fn check_cases(stress: bool) {
    let mut vm = vm(stress);
    let mut checked = 0;
    for (src, calls) in CASES {
        if calls.is_empty() {
            continue; // listed as unsupported
        }
        let f = compile(src);
        for args in *calls {
            if stress && src.contains("100000") {
                continue; // slow under stress, and allocation-free anyway
            }
            let args: Vec<Value> = args.iter().map(|&a| Value::number(a)).collect();
            let want = on_toy(&f, &args);
            let got = on_vm(&mut vm, &f, &args);
            // The toy stops at 10,000 frames; the interpreter's limit is
            // its stack size. Both throw a RangeError.
            assert_eq!(got, want, "{src} with {args:?}");
            checked += 1;
        }
    }
    assert!(checked > 80, "only {checked} calls checked");
}

#[test]
fn compiled_functions_match_the_toy_context() {
    check_cases(false);
}

#[test]
fn compiled_functions_match_the_toy_context_under_gc_stress() {
    check_cases(true);
}

#[test]
fn closures_and_environments_survive_collections() {
    // Every counter is reachable only from the heap (closure cells and
    // environments) between calls; stress mode collects at each
    // allocation.
    let f = compile(
        "function f(n) { function mk(k) { let c = k; return () => c++; } \
         let a = mk(100), b = mk(200), s = 0; \
         for (let i = 0; i < n; i++) { let g = mk(i); s += a() + b() + g(); } \
         return s; }",
    );
    let mut vm = vm(true);
    let got = on_vm(&mut vm, &f, &[Value::from_i32(50)]);
    assert_eq!(got, on_toy(&f, &[Value::from_i32(50)]));
    assert!(vm.cx.collections() > 50, "stress mode collected");
}

#[test]
fn proper_tail_calls_keep_the_stack_flat() {
    let src = "function f(n) { 'use strict'; function loop(k, acc) { return k === 0 ? acc : loop(k - 1, acc + 1); } return loop(n, 0); }";
    let f = compile(src);
    let mut vm = vm(false);
    // 64k slots hold a few thousand frames; 200,000 tail calls need one.
    assert_eq!(
        vm.run_function(&f, &[Value::from_i32(200_000)]).unwrap(),
        Outcome::Returned(Value::from_i32(200_000))
    );
}

#[test]
fn deep_recursion_is_a_range_error_and_the_vm_recovers() {
    let f = compile("function f(n) { function g(k) { return 1 + g(k + 1); } return g(0); }");
    let mut vm = vm(false);
    let base = vm.stack_top();
    assert_eq!(
        vm.run_function(&f, &[]).unwrap(),
        Outcome::Threw(Value::UNDEFINED, Some(ErrorKind::StackOverflow))
    );
    assert_eq!(vm.stack_top(), base);
    // Caught inside, the frames above the handler are gone.
    let f = compile(
        "function f(n) { function g(k) { return 1 + g(k + 1); } \
         try { g(0); } catch (e) { return n * 2; } }",
    );
    assert_eq!(
        vm.run_function(&f, &[Value::from_i32(21)]).unwrap(),
        Outcome::Returned(Value::from_i32(42))
    );
}

#[test]
fn the_interrupt_flag_terminates_and_is_not_catchable() {
    let f =
        compile("function f() { for (;;) { try { while (true) {} } catch (e) {} finally {} } }");
    let mut vm = vm(false);
    let flag = vm.interrupt.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        flag.store(true, Ordering::Relaxed);
    });
    assert_eq!(vm.run_function(&f, &[]).unwrap(), Outcome::Terminated);
    stopper.join().unwrap();
    // A call checks the flag at entry too.
    let f = compile("function f() { function g() { return 1; } return g(); }");
    assert_eq!(vm.run_function(&f, &[]).unwrap(), Outcome::Terminated);
    vm.interrupt.store(false, Ordering::Relaxed);
    assert_eq!(
        vm.run_function(&f, &[]).unwrap(),
        Outcome::Returned(Value::from_i32(1))
    );
}

#[test]
fn typeof_gives_atoms() {
    let f = compile(
        "function f(a) { const g = () => 1; return typeof g == 'function' && typeof a === 'number' && 'x' != 'y' \
         && typeof undefined === 'undefined' && typeof null === 'object' && typeof missing === 'undefined'; }",
    );
    let mut vm = vm(false);
    assert_eq!(
        vm.run_function(&f, &[Value::from_i32(1)]).unwrap(),
        Outcome::Returned(Value::TRUE)
    );
    let f = compile("function f() { const g = () => 1; return typeof g; }");
    let Outcome::Returned(v) = vm.run_function(&f, &[]).unwrap() else {
        panic!("typeof threw");
    };
    let Kind::String(r) = v.kind() else {
        panic!("not a string: {v:?}");
    };
    // Atoms live as long as the engine.
    let text = vm.with(|s, _| {
        let nogc = s.nogc();
        string::to_rust(nogc, nogc.handle(r.0).expect("a live atom"))
    });
    assert_eq!(text, "function");
}

#[test]
fn unverifiable_code_is_refused() {
    let mut vm = vm(false);
    let good = vm.load(&compile("function f(a) { return a; }")).unwrap();
    // A nested function that fails leaves nothing of its tree behind.
    let mut f = compile("function f(a) { function g() { return a; } return g; }");
    f.functions[0].code.truncate(1);
    assert!(matches!(
        vm.load(&f),
        Err(ferrojs_vm::interp::LoadError::Verify { .. })
    ));
    assert_eq!(vm.load(&compile("function h() {}")).unwrap(), good + 1);
}

#[test]
fn undeclared_globals_are_reference_errors() {
    let mut vm = vm(false);
    let f = compile("function f() { return missing; }");
    assert_eq!(
        vm.run_function(&f, &[]).unwrap(),
        Outcome::Threw(Value::UNDEFINED, Some(ErrorKind::UnresolvableReference))
    );
}

#[test]
fn values_held_only_by_environment_slots_survive() {
    // `h` is captured, so it lives only in an environment slot; the
    // closures made in the loop collect (stress mode) before it is called.
    let f = compile(
        "function f(n) { let h = () => 7; const g = () => h(); \
         let s = 0; for (let i = 0; i < n; i++) { const k = () => i; s += k(); } \
         return g() * 1000 + s; }",
    );
    let mut vm = vm(true);
    assert_eq!(
        vm.run_function(&f, &[Value::from_i32(10)]).unwrap(),
        Outcome::Returned(Value::from_i32(7045))
    );
}
