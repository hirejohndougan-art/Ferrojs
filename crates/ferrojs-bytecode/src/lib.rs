//! FerroJS bytecode: every op defined once (D-020), with the encoder,
//! decoder, disassembler and verifier generated from that definition
//! (D-019, D-031). See `docs/ARCHITECTURE.md` §4.
//!
//! ```
//! use ferrojs_bytecode::{testing, verify, Assembler, Ic, Imm, Limits, Reg};
//!
//! // r0 = 0; r1 = 10; do { r0 = r0 + r1; r1 = r1 - 1 } while (r1 > 0)
//! let mut a = Assembler::new();
//! let (sum, n, one, zero) = (Reg(0), Reg(1), Reg(2), Reg(3));
//! a.load_int(sum, Imm(0));
//! a.load_int(n, Imm(10));
//! a.load_int(one, Imm(1));
//! a.load_int(zero, Imm(0));
//! let top = a.new_label();
//! let done = a.new_label();
//! a.bind(top);
//! a.add(sum, sum, n, Ic(0));
//! a.sub(n, n, one, Ic(1));
//! a.less_than(Reg(4), zero, n, Ic(2));
//! a.jump_if_false(Reg(4), done);
//! a.jump_loop(top);
//! a.bind(done);
//! a.ret(sum);
//! let code = a.finish().unwrap();
//!
//! let limits = Limits { registers: 5, constants: 0, ics: 3 };
//! verify(&code, &limits).unwrap();
//! let mut cx = testing::ToyCtx::new(&limits);
//! let out = testing::run(&code, &mut cx, 1_000);
//! assert_eq!(out, testing::Outcome::Returned(ferrojs_value::Value::from_i32(55)));
//! ```

#[macro_use]
mod define;
mod ctx;
mod encode;
pub mod number;
mod operand;
mod ops;
pub mod testing;
mod verify;

pub use ctx::{Completion, ErrorKind, Feedback, Flow, OpCtx, Thrown};
pub use define::{Flags, OpInfo};
pub use encode::{
    decode, instructions, AsmError, Assembler, DecodeError, DecodeErrorKind, Instructions, Reader,
    Unit, PREFIX_EXTRA_WIDE, PREFIX_WIDE,
};
pub use number::{CompareOp, NumericOp, UnaryOp};
pub use operand::{
    AsmLabel, Const, Count, EnvSlot, Ic, Imm, Label, Operand, OperandKind, OperandValue, Reg, Width,
};
pub(crate) use ops::decode_operands;
pub use ops::{execute, handlers, operands, Instruction, Opcode};
pub use verify::{disassemble, verify, Limits, VerifyError, VerifyErrorKind};
