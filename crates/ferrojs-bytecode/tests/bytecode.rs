//! Tests for the generated op tables, the encoding, the assembler, the
//! verifier and the handler semantics (through the toy context).

use ferrojs_bytecode::testing::{run, Outcome, ToyCtx};
use ferrojs_bytecode::{
    decode, disassemble, instructions, operands, verify, AsmError, Assembler, Const, Count,
    DecodeErrorKind, EnvSlot, Feedback, Flags, Ic, Imm, Instruction, Label, Limits, Opcode,
    OperandKind, Reg, VerifyErrorKind, PREFIX_EXTRA_WIDE, PREFIX_WIDE,
};
use ferrojs_value::Value;

/// Small deterministic generator (no dependencies).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// A value of a random width class.
    fn unsigned(&mut self) -> u32 {
        match self.below(3) {
            0 => self.below(256) as u32,
            1 => self.below(65536) as u32,
            _ => self.next() as u32,
        }
    }
    fn signed(&mut self) -> i32 {
        match self.below(3) {
            0 => self.below(256) as i32 - 128,
            1 => self.below(65536) as i32 - 32768,
            _ => self.next() as i32,
        }
    }
}

// ---------------------------------------------------------------------------
// Metadata tables

#[test]
fn opcode_numbering_and_metadata_are_consistent() {
    let mut names = std::collections::HashSet::new();
    for (i, &op) in Opcode::ALL.iter().enumerate() {
        assert_eq!(op as usize, i);
        assert_eq!(Opcode::from_byte(i as u8), Some(op));
        assert!(names.insert(op.name()), "duplicate name {}", op.name());
        let info = op.info();
        let f = info.flags;
        let has_label = info.operands.contains(&OperandKind::Label);
        let has_ic = info.operands.contains(&OperandKind::Ic);
        assert_eq!(
            has_label,
            f.intersects(Flags::JUMP.with(Flags::BRANCH)),
            "{}: a Label operand iff jump or branch",
            op.name()
        );
        assert_eq!(has_ic, f.contains(Flags::HAS_IC), "{}: has_ic", op.name());
        assert!(!(f.contains(Flags::JUMP) && f.contains(Flags::BRANCH)));
        if f.contains(Flags::LOOP_EDGE) {
            assert!(
                f.contains(Flags::JUMP),
                "{}: loop edges are jumps",
                op.name()
            );
            assert!(
                f.contains(Flags::MAY_THROW),
                "{}: interrupts throw",
                op.name()
            );
        }
        let units: usize = info.operands.iter().map(|k| k.units()).sum();
        assert_eq!(units, info.units);
    }
    assert_eq!(Opcode::from_byte(Opcode::ALL.len() as u8), None);
    assert_eq!(Opcode::from_byte(PREFIX_WIDE), None);
    assert_eq!(Opcode::from_byte(PREFIX_EXTRA_WIDE), None);
    // Exactly one op checks interrupts on back-edges.
    let loops: Vec<_> = Opcode::ALL
        .iter()
        .filter(|op| op.flags().contains(Flags::LOOP_EDGE))
        .collect();
    assert_eq!(loops, [&Opcode::JumpLoop]);
}

// ---------------------------------------------------------------------------
// Encoding

/// A random instruction for `op`, built through decoding so this test
/// needs no per-op code: write the opcode and random units at the widest
/// width, decode, then check the narrow re-encoding round-trips.
fn random_instruction(op: Opcode, rng: &mut Rng) -> Instruction {
    let mut bytes = vec![PREFIX_EXTRA_WIDE, op as u8];
    for kind in op.info().operands {
        for _ in 0..kind.units() {
            let raw = match kind {
                OperandKind::Imm | OperandKind::Label => rng.signed() as u32,
                _ => rng.unsigned(),
            };
            bytes.extend_from_slice(&raw.to_le_bytes());
        }
    }
    let (insn, len) = decode(&bytes, 0).unwrap();
    assert_eq!(len, bytes.len());
    insn
}

#[test]
fn every_op_round_trips_at_every_width() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for &op in Opcode::ALL {
        for _ in 0..200 {
            let insn = random_instruction(op, &mut rng);
            let mut out = vec![0xAA]; // decode at a non-zero pc
            insn.encode(&mut out);
            assert_eq!(out.len() - 1, insn.encoded_len());
            let (back, len) = decode(&out, 1).unwrap();
            assert_eq!(back, insn);
            assert_eq!(len, insn.encoded_len());
            assert!(insn.to_string().starts_with(op.name()));
        }
    }
}

#[test]
fn encoder_picks_the_narrowest_width() {
    let enc = |i: Instruction| {
        let mut v = Vec::new();
        i.encode(&mut v);
        v
    };
    let mov = |d, s| {
        Instruction::Mov(operands::Mov {
            dst: Reg(d),
            src: Reg(s),
        })
    };
    let m = Opcode::Mov as u8;
    assert_eq!(enc(mov(1, 255)), [m, 1, 255]);
    assert_eq!(enc(mov(1, 256)), [PREFIX_WIDE, m, 1, 0, 0, 1]);
    assert_eq!(
        enc(mov(65536, 2)),
        [PREFIX_EXTRA_WIDE, m, 0, 0, 1, 0, 2, 0, 0, 0]
    );
    let li = |v| {
        Instruction::LoadInt(operands::LoadInt {
            dst: Reg(0),
            value: Imm(v),
        })
    };
    let l = Opcode::LoadInt as u8;
    assert_eq!(enc(li(-128)), [l, 0, 0x80]);
    assert_eq!(enc(li(127)), [l, 0, 0x7F]);
    assert_eq!(enc(li(-129)), [PREFIX_WIDE, l, 0, 0, 0x7F, 0xFF]);
    assert_eq!(enc(li(i32::MIN)).len(), 10);
    // EnvSlot is two units.
    let le = Instruction::LoadEnv(operands::LoadEnv {
        dst: Reg(3),
        slot: EnvSlot { depth: 1, slot: 2 },
    });
    assert_eq!(enc(le), [Opcode::LoadEnv as u8, 3, 1, 2]);
    // No operands: no prefix either.
    let r = enc(Instruction::Return(operands::Return { value: Reg(0) }));
    assert_eq!(r, [Opcode::Return as u8, 0]);
}

#[test]
fn decode_errors() {
    let m = Opcode::Mov as u8;
    let err = |code: &[u8]| decode(code, 0).unwrap_err().kind;
    assert_eq!(err(&[]), DecodeErrorKind::Truncated);
    assert_eq!(err(&[m, 1]), DecodeErrorKind::Truncated);
    assert_eq!(err(&[PREFIX_WIDE]), DecodeErrorKind::Truncated);
    assert_eq!(err(&[PREFIX_WIDE, m, 1, 0, 2]), DecodeErrorKind::Truncated);
    assert_eq!(
        err(&[PREFIX_WIDE, PREFIX_WIDE, m]),
        DecodeErrorKind::DoublePrefix
    );
    assert_eq!(
        err(&[PREFIX_EXTRA_WIDE, PREFIX_WIDE]),
        DecodeErrorKind::DoublePrefix
    );
    let bad = Opcode::ALL.len() as u8;
    assert_eq!(err(&[bad]), DecodeErrorKind::BadOpcode(bad));
    let e = decode(&[m, 1, 2, bad], 3).unwrap_err();
    assert_eq!(e.pc, 3);
    // The iterator stops at the first error.
    let items: Vec<_> = instructions(&[m, 1, 2, bad, m, 0, 0]).collect();
    assert_eq!(items.len(), 2);
    assert!(items[0].is_ok() && items[1].is_err());
}

// ---------------------------------------------------------------------------
// Assembler

fn targets(code: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for item in instructions(code) {
        let (pc, insn) = item.unwrap();
        insn.for_each_operand(&mut |v| {
            if let ferrojs_bytecode::OperandValue::Label(Label(off)) = v {
                out.push((pc, (pc as i64 + off as i64) as usize));
            }
        });
    }
    out
}

fn starts(code: &[u8]) -> Vec<usize> {
    instructions(code).map(|i| i.unwrap().0).collect()
}

#[test]
fn labels_resolve_and_widen() {
    // A forward jump over ~300 bytes needs a wide offset; a backward one
    // over the same body does too. Short jumps stay narrow.
    let mut a = Assembler::new();
    let end = a.new_label();
    let top = a.new_label();
    let near = a.new_label();
    a.jump_if_true(Reg(0), near);
    a.bind(near);
    a.bind(top);
    a.jump_if_false(Reg(0), end);
    for _ in 0..100 {
        a.mov(Reg(1), Reg(2));
    }
    a.jump_loop(top);
    a.bind(end);
    a.ret(Reg(1));
    let code = a.finish().unwrap();
    let s = starts(&code);
    let t = targets(&code);
    assert_eq!(t.len(), 3);
    assert_eq!(t[0], (0, s[1])); // near, narrow
    assert_eq!(code[0], Opcode::JumpIfTrue as u8);
    assert_eq!(code[s[1]], PREFIX_WIDE); // the jump to `end` widened
    assert_eq!(t[1], (s[1], *s.last().unwrap()));
    let back = s[s.len() - 2];
    assert_eq!(code[back], PREFIX_WIDE);
    assert_eq!(t[2], (back, s[1]));
    let limits = Limits {
        registers: 3,
        constants: 0,
        ics: 0,
    };
    verify(&code, &limits).unwrap();
}

#[test]
fn widening_reaches_a_fixed_point() {
    // Offsets near the narrow limit: widening one jump moves the others.
    for n in 55..70 {
        let mut a = Assembler::new();
        let labels: Vec<_> = (0..4).map(|_| a.new_label()).collect();
        for &l in &labels {
            a.jump_if_true(Reg(0), l);
        }
        for _ in 0..n {
            a.mov(Reg(0), Reg(0));
        }
        for &l in &labels {
            a.bind(l);
            a.mov(Reg(0), Reg(0));
        }
        a.ret(Reg(0));
        let code = a.finish().unwrap();
        let s = starts(&code);
        let t = targets(&code);
        let binds = &s[4 + n..4 + n + 4];
        for (i, &(_, target)) in t.iter().enumerate() {
            assert_eq!(target, binds[i], "n = {n}, jump {i}");
        }
        verify(
            &code,
            &Limits {
                registers: 1,
                constants: 0,
                ics: 0,
            },
        )
        .unwrap();
    }
}

#[test]
fn unbound_label_is_an_error() {
    let mut a = Assembler::new();
    let l = a.new_label();
    let _unused = a.new_label();
    a.jump(l);
    assert_eq!(a.finish().unwrap_err(), AsmError::UnboundLabel(l));
}

#[test]
#[should_panic(expected = "bound twice")]
fn binding_twice_panics() {
    let mut a = Assembler::new();
    let l = a.new_label();
    a.bind(l);
    a.bind(l);
}

// ---------------------------------------------------------------------------
// Verifier

fn limits(registers: u32, constants: u32, ics: u32) -> Limits {
    Limits {
        registers,
        constants,
        ics,
    }
}

fn verify_err(code: &[u8], l: Limits) -> (usize, VerifyErrorKind) {
    let e = verify(code, &l).unwrap_err();
    (e.pc, e.kind)
}

#[test]
fn verifier_checks_ranges() {
    let mut a = Assembler::new();
    a.add(Reg(0), Reg(1), Reg(2), Ic(3));
    a.load_const(Reg(0), Const(4));
    a.ret(Reg(0));
    let code = a.finish().unwrap();
    verify(&code, &limits(3, 5, 4)).unwrap();
    assert_eq!(
        verify_err(&code, limits(2, 5, 4)),
        (0, VerifyErrorKind::RegisterOutOfRange(2))
    );
    assert_eq!(
        verify_err(&code, limits(3, 5, 3)),
        (0, VerifyErrorKind::IcOutOfRange(3))
    );
    assert_eq!(
        verify_err(&code, limits(3, 4, 4)),
        (5, VerifyErrorKind::ConstantOutOfRange(4))
    );
}

#[test]
fn verifier_checks_control_flow() {
    let l = limits(1, 0, 0);
    let jif = Opcode::JumpIfTrue as u8;
    let jmp = Opcode::Jump as u8;
    let jl = Opcode::JumpLoop as u8;
    let ret = Opcode::Return as u8;
    let mov = Opcode::Mov as u8;
    assert_eq!(verify_err(&[], l), (0, VerifyErrorKind::Empty));
    assert_eq!(
        verify_err(&[mov, 0, 0], l),
        (0, VerifyErrorKind::FallsOffEnd)
    );
    assert_eq!(
        verify_err(&[jif, 0, 3, mov, 0, 0], l),
        (3, VerifyErrorKind::FallsOffEnd)
    );
    // Into the middle of `Mov` at 3.
    assert_eq!(
        verify_err(&[jif, 0, 4, mov, 0, 0, ret, 0], l),
        (0, VerifyErrorKind::JumpIntoInstruction(4))
    );
    // Past the end, and before the start.
    assert_eq!(
        verify_err(&[jmp, 2], l),
        (0, VerifyErrorKind::JumpOutOfBounds(2))
    );
    assert_eq!(
        verify_err(&[ret, 0, jl, 0xFC], l),
        (2, VerifyErrorKind::JumpOutOfBounds(-2))
    );
    // Backward plain jump, self-jump, forward loop edge.
    assert_eq!(
        verify_err(&[ret, 0, jmp, 0xFE], l),
        (2, VerifyErrorKind::BackwardJumpWithoutLoopEdge)
    );
    assert_eq!(
        verify_err(&[jmp, 0], l),
        (0, VerifyErrorKind::BackwardJumpWithoutLoopEdge)
    );
    assert_eq!(
        verify_err(&[jl, 2, ret, 0], l),
        (0, VerifyErrorKind::ForwardLoopEdge)
    );
    // A loop edge to itself is fine (it checks interrupts every time).
    verify(&[jl, 0], &l).unwrap();
    verify(&[ret, 0, jl, 0xFE], &l).unwrap();
    // A jump to a prefixed instruction must target the prefix.
    let wide_mov = [PREFIX_WIDE, mov, 0, 0, 0, 0];
    let mut code = vec![jif, 0, 3];
    code.extend_from_slice(&wide_mov);
    code.extend_from_slice(&[ret, 0]);
    verify(&code, &l).unwrap();
    code[2] = 4;
    assert_eq!(
        verify_err(&code, l),
        (0, VerifyErrorKind::JumpIntoInstruction(4))
    );
    // Decode errors are reported where they happen.
    assert_eq!(
        verify_err(&[ret, 0, 0xF0], l),
        (2, VerifyErrorKind::Decode(DecodeErrorKind::BadOpcode(0xF0)))
    );
}

#[test]
fn disassembly() {
    let mut a = Assembler::new();
    let done = a.new_label();
    a.load_int(Reg(0), Imm(-3));
    a.jump_if_false(Reg(0), done);
    a.load_env(
        Reg(1),
        EnvSlot {
            depth: 0,
            slot: 300,
        },
    );
    a.bind(done);
    a.ret(Reg(0));
    let text = disassemble(&a.finish().unwrap()).unwrap();
    assert_eq!(
        text,
        "0000: LoadInt r0, -3\n\
         0003: JumpIfFalse r0, +11  ; -> 0014\n\
         0006: LoadEnv r1, env[0:300]\n\
         0014: Return r0\n"
    );
    assert_eq!(format!("{}", Count(4)), "#4");
    assert_eq!(
        format!("{}", Flags::JUMP.with(Flags::MAY_GC)),
        "may_gc, jump"
    );
}

// ---------------------------------------------------------------------------
// Handler semantics

fn exec(
    build: impl FnOnce(&mut Assembler),
    l: Limits,
    setup: impl FnOnce(&mut ToyCtx),
) -> (Outcome, ToyCtx) {
    let mut a = Assembler::new();
    build(&mut a);
    let code = a.finish().unwrap();
    verify(&code, &l).unwrap();
    let mut cx = ToyCtx::new(&l);
    setup(&mut cx);
    let out = run(&code, &mut cx, 10_000);
    (out, cx)
}

fn binop(
    op: fn(&mut Assembler, Reg, Reg, Reg, Ic),
    a: Value,
    b: Value,
) -> (Outcome, Feedback, u64) {
    let (out, cx) = exec(
        |asm| {
            op(asm, Reg(2), Reg(0), Reg(1), Ic(0));
            asm.ret(Reg(2));
        },
        limits(3, 0, 1),
        |cx| {
            cx.regs[0] = a;
            cx.regs[1] = b;
        },
    );
    (out, cx.feedback[0], cx.slow_calls)
}

fn bits(o: Outcome) -> u64 {
    match o {
        Outcome::Returned(v) => v.to_bits(),
        other => panic!("expected a return, got {other:?}"),
    }
}

#[test]
fn arithmetic_fast_paths() {
    let i = Value::from_i32;
    let d = Value::from_f64;
    let (o, fb, slow) = binop(Assembler::add, i(2), i(3));
    assert_eq!((o, fb, slow), (Outcome::Returned(i(5)), Feedback::INT32, 0));
    // Overflow goes to the double path.
    let (o, fb, _) = binop(Assembler::add, i(i32::MAX), i(1));
    assert_eq!(o, Outcome::Returned(d(2147483648.0)));
    assert_eq!(fb, Feedback::NUMBER);
    let (o, _, _) = binop(Assembler::sub, i(i32::MIN), i(1));
    assert_eq!(o, Outcome::Returned(d(-2147483649.0)));
    // Integral double results come back as int32.
    let (o, fb, _) = binop(Assembler::add, d(1.5), d(1.5));
    assert_eq!((o, fb), (Outcome::Returned(i(3)), Feedback::NUMBER));
    // 0 * -5 and -5 * 0 are -0, which int32 can't hold.
    for (a, b) in [(0, -5), (-5, 0)] {
        let (o, _, _) = binop(Assembler::mul, i(a), i(b));
        assert_eq!(bits(o), d(-0.0).to_bits());
    }
    let (o, fb, _) = binop(Assembler::mul, i(0), i(5));
    assert_eq!((o, fb), (Outcome::Returned(i(0)), Feedback::INT32));
    let (o, _, _) = binop(Assembler::mul, i(65536), i(65536));
    assert_eq!(o, Outcome::Returned(d(4294967296.0)));
    // NaN stays canonical.
    let (o, _, _) = binop(Assembler::sub, d(f64::INFINITY), d(f64::INFINITY));
    assert_eq!(bits(o), d(f64::NAN).to_bits());
    // Non-numbers take the slow path: `undefined + 1` is NaN, and heap
    // values (here a fake object) throw in the toy.
    let (o, fb, slow) = binop(Assembler::add, Value::UNDEFINED, i(1));
    assert_eq!(
        (bits(o), fb, slow),
        (d(f64::NAN).to_bits(), Feedback::OTHER, 1)
    );
    let obj = Value::object(ferrojs_value::HeapRef(64));
    let (o, fb, slow) = binop(Assembler::add, obj, i(1));
    assert_eq!(
        (o, fb, slow),
        (Outcome::Threw(Value::UNDEFINED), Feedback::OTHER, 1)
    );
}

#[test]
fn feedback_accumulates() {
    let l = limits(3, 0, 1);
    let (_, cx) = exec(
        |a| {
            a.add(Reg(2), Reg(0), Reg(0), Ic(0));
            a.add(Reg(2), Reg(1), Reg(0), Ic(0));
            a.ret(Reg(2));
        },
        l,
        |cx| {
            cx.regs[0] = Value::from_i32(1);
            cx.regs[1] = Value::from_f64(0.5);
        },
    );
    assert!(cx.feedback[0].contains(Feedback::INT32.union(Feedback::NUMBER)));
    assert!(!cx.feedback[0].contains(Feedback::OTHER));
}

#[test]
fn comparisons() {
    let i = Value::from_i32;
    let d = Value::from_f64;
    let t = Outcome::Returned(Value::TRUE);
    let f = Outcome::Returned(Value::FALSE);
    assert_eq!(binop(Assembler::less_than, i(1), i(2)).0, t);
    assert_eq!(binop(Assembler::less_than, i(2), i(2)).0, f);
    assert_eq!(binop(Assembler::less_than, d(0.5), i(1)).0, t);
    assert_eq!(binop(Assembler::less_than, d(f64::NAN), i(1)).0, f);
    assert_eq!(binop(Assembler::less_than, i(1), d(f64::NAN)).0, f);
    let seq = |a: Value, b: Value| {
        exec(
            |asm| {
                asm.strict_equal(Reg(2), Reg(0), Reg(1));
                asm.ret(Reg(2));
            },
            limits(3, 0, 0),
            |cx| {
                cx.regs[0] = a;
                cx.regs[1] = b;
            },
        )
        .0
    };
    assert_eq!(seq(d(f64::NAN), d(f64::NAN)), f);
    assert_eq!(seq(d(-0.0), i(0)), t);
    assert_eq!(seq(d(2.0), i(2)), t);
    assert_eq!(seq(Value::NULL, Value::NULL), t);
    assert_eq!(seq(Value::NULL, Value::UNDEFINED), f);
    assert_eq!(seq(i(1), Value::TRUE), f);
}

#[test]
fn not_and_branches_use_to_boolean() {
    for (v, truthy) in [
        (Value::TRUE, true),
        (Value::FALSE, false),
        (Value::UNDEFINED, false),
        (Value::NULL, false),
        (Value::from_i32(0), false),
        (Value::from_i32(-1), true),
        (Value::from_f64(-0.0), false),
        (Value::from_f64(f64::NAN), false),
        (Value::from_f64(0.25), true),
    ] {
        let (o, _) = exec(
            |a| {
                a.not(Reg(1), Reg(0));
                a.ret(Reg(1));
            },
            limits(2, 0, 0),
            |cx| cx.regs[0] = v,
        );
        assert_eq!(o, Outcome::Returned(Value::from_bool(!truthy)), "!{v:?}");
        for if_true in [true, false] {
            let (o, _) = exec(
                |a| {
                    let taken = a.new_label();
                    if if_true {
                        a.jump_if_true(Reg(0), taken);
                    } else {
                        a.jump_if_false(Reg(0), taken);
                    }
                    a.load_int(Reg(1), Imm(0));
                    a.ret(Reg(1));
                    a.bind(taken);
                    a.load_int(Reg(1), Imm(1));
                    a.ret(Reg(1));
                },
                limits(2, 0, 0),
                |cx| cx.regs[0] = v,
            );
            let jumped = truthy == if_true;
            assert_eq!(o, Outcome::Returned(Value::from_i32(jumped as i32)));
        }
    }
}

#[test]
fn loads_moves_and_constants() {
    let (o, cx) = exec(
        |a| {
            a.load_undefined(Reg(0));
            a.load_null(Reg(1));
            a.load_true(Reg(2));
            a.load_false(Reg(3));
            a.load_int(Reg(4), Imm(-70000));
            a.load_const(Reg(5), Const(1));
            a.mov(Reg(6), Reg(5));
            a.ret(Reg(6));
        },
        limits(7, 2, 0),
        |cx| {
            cx.regs.fill(Value::from_i32(9));
            cx.constants[1] = Value::from_f64(2.5);
        },
    );
    assert_eq!(o, Outcome::Returned(Value::from_f64(2.5)));
    assert_eq!(
        cx.regs[..5],
        [
            Value::UNDEFINED,
            Value::NULL,
            Value::TRUE,
            Value::FALSE,
            Value::from_i32(-70000)
        ]
    );
}

#[test]
fn environments() {
    let (o, cx) = exec(
        |a| {
            a.load_env(Reg(0), EnvSlot { depth: 1, slot: 0 });
            a.store_env(EnvSlot { depth: 0, slot: 1 }, Reg(0));
            a.load_env(Reg(1), EnvSlot { depth: 2, slot: 0 });
            a.ret(Reg(1));
        },
        limits(2, 0, 0),
        |cx| cx.envs = vec![vec![Value::from_i32(7)], vec![Value::NULL, Value::NULL]],
    );
    // depth 2 doesn't exist: the toy's env_get throws.
    assert_eq!(o, Outcome::Threw(Value::UNDEFINED));
    assert_eq!(cx.envs[1][1], Value::from_i32(7));
    assert_eq!(cx.regs[0], Value::from_i32(7));
}

#[test]
fn throw_and_interrupts() {
    let (o, _) = exec(
        |a| {
            a.load_int(Reg(0), Imm(42));
            a.throw(Reg(0));
        },
        limits(1, 0, 0),
        |_| {},
    );
    assert_eq!(o, Outcome::Threw(Value::from_i32(42)));
    // An infinite loop ends at the interrupt check.
    let (o, cx) = exec(
        |a| {
            let top = a.new_label();
            a.bind(top);
            a.mov(Reg(0), Reg(0));
            a.jump_loop(top);
        },
        limits(1, 0, 0),
        |cx| cx.interrupt_after = Some(5),
    );
    assert_eq!(o, Outcome::Threw(Value::NULL));
    assert_eq!(cx.interrupt_checks, 5);
}

// ---------------------------------------------------------------------------
// Verifier soundness smoke test (the fuzz target does this at length)

#[test]
fn random_bytes_never_break_verified_execution() {
    let mut rng = Rng(12345);
    let mut verified = 0;
    let ops = Opcode::ALL.len() as u64;
    for _ in 0..20_000 {
        // Mostly well-formed instructions with small operands, so a good
        // share verifies, plus some raw bytes.
        let mut code = Vec::new();
        for _ in 0..1 + rng.below(8) {
            if rng.below(10) == 0 {
                code.push(rng.next() as u8);
                continue;
            }
            let op = Opcode::ALL[rng.below(ops) as usize];
            let wide = rng.below(8) == 0;
            if wide {
                code.push(PREFIX_WIDE);
            }
            code.push(op as u8);
            for kind in op.info().operands {
                for _ in 0..kind.units() {
                    let v = match kind {
                        OperandKind::Label => rng.below(13) as i8 - 6,
                        _ => rng.below(5) as i8,
                    };
                    code.push(v as u8);
                    if wide {
                        code.push(if v < 0 { 0xFF } else { 0 });
                    }
                }
            }
        }
        if rng.below(4) != 0 {
            code.extend_from_slice(&[Opcode::Return as u8, 0]);
        }
        let l = limits(4, 2, 2);
        if verify(&code, &l).is_ok() {
            verified += 1;
            disassemble(&code).unwrap();
            let mut cx = ToyCtx::new(&l);
            cx.envs = vec![vec![Value::UNDEFINED; 2]];
            cx.interrupt_after = Some(100);
            let _ = run(&code, &mut cx, 1_000);
        }
    }
    assert!(verified > 500, "only {verified} random programs verified");
}

#[test]
fn exception_tables_are_verified() {
    use ferrojs_bytecode::{verify_handlers, Handler};
    // 0: LoadInt r0, 1 (3 bytes)  3: Throw r0 (2)  5: Return r0 (2)
    let mut a = Assembler::new();
    a.load_int(Reg(0), Imm(1));
    a.throw(Reg(0));
    a.ret(Reg(0));
    let code = a.finish().unwrap();
    let limits = limits(2, 0, 0);
    verify(&code, &limits).unwrap();
    let h = |start, end, target, r| Handler {
        start,
        end,
        target,
        exception: Reg(r),
    };
    assert!(verify_handlers(&code, &[h(0, 5, 5, 1)], &limits).is_ok());
    assert!(verify_handlers(&code, &[h(3, 5, 5, 0)], &limits).is_ok());
    for bad in [
        h(0, 0, 5, 1), // empty range
        h(0, 5, 3, 1), // target inside the range: could loop without an interrupt check
        h(0, 4, 5, 1), // end inside an instruction
        h(1, 5, 5, 1), // start inside an instruction
        h(0, 5, 6, 1), // target inside an instruction
        h(0, 5, 7, 1), // target past the end
        h(0, 5, 5, 2), // register out of range
    ] {
        assert!(verify_handlers(&code, &[bad], &limits).is_err(), "{bad:?}");
    }

    // The toy unwinds to the handler, storing the exception.
    let mut cx = ToyCtx::new(&limits);
    let out = ferrojs_bytecode::testing::run_with_handlers(&code, &[h(0, 5, 5, 1)], &mut cx, 100);
    assert_eq!(out, Outcome::Returned(Value::from_i32(1)));
    assert_eq!(cx.regs[1], Value::from_i32(1));
}
