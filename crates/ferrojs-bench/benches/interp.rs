//! The interpreter (task 1.7b, D-047): a compiled loop with a call per
//! iteration, run on the real frame stack. Dispatch cost must not grow
//! with the number of ops (D-036).
//!
//! Linux-only like the other benchmarks: every item is behind
//! `cfg(target_os = "linux")`, and other platforms get an empty `main`.

#[cfg(target_os = "linux")]
use std::hint::black_box;

#[cfg(target_os = "linux")]
use ferrojs_compiler::compile_function_source;
#[cfg(target_os = "linux")]
use ferrojs_value::Value;
#[cfg(target_os = "linux")]
use ferrojs_vm::interp::Outcome;
#[cfg(target_os = "linux")]
use ferrojs_vm::{Vm, VmOptions};
#[cfg(target_os = "linux")]
use gungraun::prelude::*;

/// The engine with the function loaded, and its template index.
#[cfg(target_os = "linux")]
fn loop_with_calls() -> (Vm, u32) {
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
    let f = compile_function_source(
        "function f(n) { function sq(x) { return x * x; } \
         let s = 0; for (let i = 0; i < n; i++) s += sq(i) % 7; return s; }",
    )
    .expect("compiles");
    let t = vm.load(&f).expect("loads");
    (vm, t)
}

// 1,000 iterations, each with a call and a return.
#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::loop_with_calls(setup = loop_with_calls)]
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
