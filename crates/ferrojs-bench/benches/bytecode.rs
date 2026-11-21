//! Bytecode micro-benchmarks (D-019, D-020, D-031): decoding, and the
//! decode-and-dispatch loop over the generated handlers, which the
//! interpreter (task 1.7) will build on.
//!
//! Linux-only like the other benchmarks: every item is behind
//! `cfg(target_os = "linux")`, and other platforms get an empty `main`.

#[cfg(target_os = "linux")]
use std::hint::black_box;

#[cfg(target_os = "linux")]
use ferrojs_bytecode::testing::{run, Outcome, ToyCtx};
#[cfg(target_os = "linux")]
use ferrojs_bytecode::{instructions, Assembler, EnvSlot, Ic, Imm, Limits, Reg};
#[cfg(target_os = "linux")]
use gungraun::prelude::*;

#[cfg(target_os = "linux")]
const LIMITS: Limits = Limits {
    registers: 300,
    constants: 0,
    ics: 3,
};

/// `sum = 0; for (n = 1000; 0 < n; n--) sum += n; return sum`.
#[cfg(target_os = "linux")]
fn count_loop() -> (Vec<u8>, ToyCtx) {
    let mut a = Assembler::new();
    let (sum, n, one, zero, t) = (Reg(0), Reg(1), Reg(2), Reg(3), Reg(4));
    a.load_int(sum, Imm(0));
    a.load_int(n, Imm(1000));
    a.load_int(one, Imm(1));
    a.load_int(zero, Imm(0));
    let top = a.new_label();
    let done = a.new_label();
    a.bind(top);
    a.less_than(t, zero, n, Ic(0));
    a.jump_if_false(t, done);
    a.add(sum, sum, n, Ic(1));
    a.sub(n, n, one, Ic(2));
    a.jump_loop(top);
    a.bind(done);
    a.ret(sum);
    (a.finish().expect("assembles"), ToyCtx::new(&LIMITS))
}

/// 1024 instructions of mixed ops and widths.
#[cfg(target_os = "linux")]
fn mixed_code() -> Vec<u8> {
    let mut a = Assembler::new();
    for i in 0..256u32 {
        a.mov(Reg(i % 300), Reg((i * 7) % 300));
        a.add(Reg(1), Reg(2), Reg(3), Ic(i % 3));
        a.load_int(Reg(4), Imm(i as i32 * 1000 - 50_000));
        a.load_env(
            Reg(5),
            EnvSlot {
                depth: i % 3,
                slot: i,
            },
        );
    }
    a.ret(Reg(0));
    a.finish().expect("assembles")
}

#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::mixed(setup = mixed_code)]
fn decode(code: Vec<u8>) -> usize {
    let mut acc = 0usize;
    for item in instructions(black_box(&code)) {
        let (pc, insn) = item.expect("decodes");
        acc = acc.wrapping_add(pc ^ insn.opcode() as usize);
    }
    acc
}

#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::count_loop(setup = count_loop)]
fn dispatch((code, mut cx): (Vec<u8>, ToyCtx)) -> Outcome {
    run(black_box(&code), &mut cx, 100_000)
}

#[cfg(target_os = "linux")]
library_benchmark_group!(name = bytecode, benchmarks = [decode, dispatch]);

#[cfg(target_os = "linux")]
main!(library_benchmark_groups = bytecode);

#[cfg(not(target_os = "linux"))]
fn main() {}
