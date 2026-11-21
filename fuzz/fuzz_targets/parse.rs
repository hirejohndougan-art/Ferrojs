//! Untrusted source text through the front end (D-018, D-033): oxc's
//! parser, oxc_semantic's early errors, our early-error pass, and the
//! bytecode compiler. The first byte picks the goal (script or module); the
//! rest must be UTF-8, like any source the embedder hands us after
//! decoding. Nothing may panic, every rejection must carry an error, and
//! every function the compiler accepts must pass the bytecode verifier and
//! run on the toy context without panicking, nested functions included.
#![no_main]

use ferrojs_bytecode::testing::{run_program, ToyCtx, ToyFunction};
use ferrojs_bytecode::{verify, verify_handlers};
use ferrojs_compiler::{compile_function, parse, Allocator, CompiledFunction, Goal};
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
        let mut program = Vec::new();
        flatten(&compiled, &mut program);
        let mut cx = ToyCtx::default();
        let args = [Value::from_i32(i32::from(*g)); 2];
        run_program(&program, 0, &args, &mut cx, 10_000);
    }
});

/// Verifies every function of the tree and flattens it into a toy program
/// (root at index 0).
fn flatten(f: &CompiledFunction, out: &mut Vec<ToyFunction>) -> usize {
    let limits = f.limits();
    verify(&f.code, &limits).expect("compiler output verifies");
    verify_handlers(&f.code, &f.handlers, &limits).expect("compiler exception tables verify");
    let index = out.len();
    out.push(ToyFunction::default());
    let functions = f.functions.iter().map(|g| flatten(g, out)).collect();
    out[index] = ToyFunction {
        code: f.code.clone(),
        handlers: f.handlers.clone(),
        constants: f.constant_values(),
        limits,
        params: f.params,
        functions,
    };
    index
}
