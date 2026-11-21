//! Untrusted source text through the front end (D-018, D-033): oxc's
//! parser, oxc_semantic's early errors, our early-error pass, and the
//! bytecode compiler. The first byte picks the goal (script or module); the
//! rest must be UTF-8, like any source the embedder hands us after
//! decoding. Nothing may panic, every rejection must carry an error, and
//! every function the compiler accepts must pass the bytecode verifier and
//! run on the toy context without panicking.
#![no_main]

use ferrojs_bytecode::testing::{run, ToyCtx};
use ferrojs_bytecode::verify;
use ferrojs_compiler::{compile_function, parse, Allocator, Goal};
use ferrojs_value::Value;
use libfuzzer_sys::fuzz_target;
use oxc_ast::ast::Statement;

fuzz_target!(|data: &[u8]| {
    let [g, rest @ ..] = data else {
        return;
    };
    let Ok(source) = std::str::from_utf8(rest) else {
        return;
    };
    let goal = if g & 1 == 0 {
        Goal::Script
    } else {
        Goal::Module
    };
    let allocator = Allocator::default();
    let parsed = match parse(&allocator, source, goal) {
        Ok(p) => p,
        Err(errors) => {
            assert!(!errors.is_empty(), "a rejection must carry an error");
            return;
        }
    };
    for stmt in &parsed.program.body {
        let Statement::FunctionDeclaration(func) = stmt else {
            continue;
        };
        let Ok(compiled) = compile_function(&parsed, func) else {
            continue;
        };
        let limits = compiled.limits();
        verify(&compiled.code, &limits).expect("compiler output verifies");
        let mut cx = ToyCtx::new(&limits);
        cx.constants = compiled.constant_values();
        for r in cx.regs.iter_mut().take(compiled.params as usize) {
            *r = Value::from_i32(i32::from(*g));
        }
        run(&compiled.code, &mut cx, 10_000);
    }
});
