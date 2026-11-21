//! Untrusted bytecode through the verifier (D-019, D-031).
//!
//! The first three bytes pick the limits (registers, constants, feedback
//! slots); the rest is the code. Verification must never panic. Code it
//! accepts must disassemble, re-encode instruction by instruction into
//! code that decodes to the same instructions, and run on the toy context
//! without panicking: an out-of-range register, a jump into the middle of
//! an instruction or off the end would panic there.
#![no_main]

use ferrojs_bytecode::testing::{run, ToyCtx};
use ferrojs_bytecode::{decode, disassemble, instructions, verify, Limits};
use ferrojs_value::Value;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let [r, c, i, code @ ..] = data else {
        return;
    };
    let limits = Limits {
        registers: u32::from(*r % 16),
        constants: u32::from(*c % 4),
        ics: u32::from(*i % 4),
        functions: u32::from(*i / 4 % 4),
    };
    if verify(code, &limits).is_err() {
        return;
    }
    disassemble(code).expect("verified code disassembles");
    for item in instructions(code) {
        let (_, insn) = item.expect("verified code decodes");
        let mut out = Vec::new();
        insn.encode(&mut out);
        assert_eq!(decode(&out, 0).expect("re-encoded"), (insn, out.len()));
    }
    let mut cx = ToyCtx::new(&limits);
    cx.set_env_chain(vec![vec![Value::UNDEFINED; 4]; 2]);
    cx.interrupt_after = Some(1_000);
    let _ = run(code, &mut cx, 10_000);
});
