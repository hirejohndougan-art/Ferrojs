//! The interpreter (task 1.7b, D-047): a compiled loop with a call per
//! iteration, run on the real frame stack. Dispatch cost must not grow
//! with the number of ops (D-036). Task 1.7c (D-050) adds a script loop on
//! global variables with string concatenation; task 1.7e (D-052) a loop
//! of property reads and writes.
//!
//! Linux-only like the other benchmarks: every item is behind
//! `cfg(target_os = "linux")`, and other platforms get an empty `main`.

#[cfg(target_os = "linux")]
use std::hint::black_box;

#[cfg(target_os = "linux")]
use ferrojs_compiler::{compile_function_source, compile_script_source, CompiledFunction};
#[cfg(target_os = "linux")]
use ferrojs_value::Value;
#[cfg(target_os = "linux")]
use ferrojs_vm::interp::Outcome;
#[cfg(target_os = "linux")]
use ferrojs_vm::{Vm, VmOptions};
#[cfg(target_os = "linux")]
use gungraun::prelude::*;

/// The engine with `f` loaded, and its template index.
#[cfg(target_os = "linux")]
fn loaded(f: &CompiledFunction) -> (Vm, u32) {
    let mut vm = Vm::new(VmOptions {
        cx: ferrojs_core::CxOptions { gc_stress: false },
        heap: ferrojs_core::HeapOptions {
            poison: false,
            verify: false,
        },
        stack_slots: 1 << 12,
        ..VmOptions::default()
    })
    .expect("a heap");
    let t = vm.load(f).expect("loads");
    (vm, t)
}

#[cfg(target_os = "linux")]
fn loop_with_calls() -> (Vm, u32) {
    loaded(
        &compile_function_source(
            "function f(n) { function sq(x) { return x * x; } \
             let s = 0; for (let i = 0; i < n; i++) s += sq(i) % 7; return s; }",
        )
        .expect("compiles"),
    )
}

/// A script: 200 iterations over global `var`s and a global `let`, each
/// with a string concatenation (the argument is ignored).
#[cfg(target_os = "linux")]
fn global_strings() -> (Vm, u32) {
    loaded(
        &compile_script_source(
            "var s = '', n = 0; let k = 3; \
             for (var i = 0; i < 200; i++) { n = n + i * k; s = s + 'x'; }",
        )
        .expect("compiles"),
    )
}

/// A function: 1,000 iterations of named and keyed property reads and
/// writes on a small object, plus a method call.
#[cfg(target_os = "linux")]
fn properties() -> (Vm, u32) {
    loaded(
        &compile_function_source(
            "function f(n) { 'use strict'; \
             const o = { x: 0, y: 1, k: { v: 2 }, m() { return this.y; } }; \
             for (let i = 0; i < n; i++) { o.x = o.x + o.k.v; o['y'] += i % 3; o[i % 8] = i; } \
             return o.x + o.m() + o[3]; }",
        )
        .expect("compiles"),
    )
}

// loop_with_calls: 1,000 iterations, each with a call and a return.
#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::loop_with_calls(setup = loop_with_calls)]
#[bench::global_strings(setup = global_strings)]
#[bench::properties(setup = properties)]
fn run(setup: (Vm, u32)) -> Outcome {
    work(black_box(setup))
}

#[cfg(target_os = "linux")]
#[inline(never)]
fn work((mut vm, t): (Vm, u32)) -> Outcome {
    vm.call_template(t, Value::UNDEFINED, &[Value::from_i32(1000)])
}

#[cfg(target_os = "linux")]
library_benchmark_group!(name = interp, benchmarks = [run]);

#[cfg(target_os = "linux")]
main!(library_benchmark_groups = interp);

#[cfg(not(target_os = "linux"))]
fn main() {}
