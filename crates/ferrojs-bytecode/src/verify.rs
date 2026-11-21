//! The bytecode verifier (D-019, ARCHITECTURE.md §4.1) and disassembler.
//!
//! Bytecode may later come from cache files, so it is untrusted. Verified
//! code satisfies, for every instruction:
//!
//! - it decodes (valid opcode, not truncated, at most one prefix);
//! - every `Reg` is below the register count, every `Const` below the
//!   constant count, every `Ic` below the feedback-slot count;
//! - every jump target lands on the first byte of an instruction (its
//!   prefix, if any) inside the code;
//! - only `loop_edge` ops jump backward (offset ≤ 0), and they never jump
//!   forward, so every loop passes an interrupt check (D-007);
//! - the last instruction never falls through.
//!
//! `EnvSlot` depths and slots are not checked here: environments are
//! runtime objects, and `env_get` / `env_set` check them.

use core::fmt;
use core::fmt::Write as _;

use crate::encode::{instructions, DecodeError, DecodeErrorKind};
use crate::operand::OperandValue;
use crate::{Flags, Opcode};

/// Sizes the verifier checks operands against.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Limits {
    pub registers: u32,
    pub constants: u32,
    pub ics: u32,
}

/// Why bytecode failed verification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VerifyErrorKind {
    Decode(DecodeErrorKind),
    Empty,
    RegisterOutOfRange(u32),
    ConstantOutOfRange(u32),
    IcOutOfRange(u32),
    /// The target is outside the code.
    JumpOutOfBounds(i64),
    /// The target is inside an instruction.
    JumpIntoInstruction(usize),
    /// A non-loop-edge jump with offset ≤ 0.
    BackwardJumpWithoutLoopEdge,
    /// A loop-edge jump with offset > 0.
    ForwardLoopEdge,
    /// The last instruction falls through past the end.
    FallsOffEnd,
}

/// A verification failure at instruction `pc`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VerifyError {
    pub pc: usize,
    pub kind: VerifyErrorKind,
}

impl From<DecodeError> for VerifyError {
    fn from(e: DecodeError) -> Self {
        VerifyError {
            pc: e.pc,
            kind: VerifyErrorKind::Decode(e.kind),
        }
    }
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}: ", self.pc)?;
        match self.kind {
            VerifyErrorKind::Decode(k) => write!(
                f,
                "{}",
                DecodeError {
                    pc: self.pc,
                    kind: k
                }
            ),
            VerifyErrorKind::Empty => write!(f, "empty bytecode"),
            VerifyErrorKind::RegisterOutOfRange(r) => write!(f, "register r{r} out of range"),
            VerifyErrorKind::ConstantOutOfRange(c) => write!(f, "constant c{c} out of range"),
            VerifyErrorKind::IcOutOfRange(i) => write!(f, "feedback slot ic{i} out of range"),
            VerifyErrorKind::JumpOutOfBounds(t) => write!(f, "jump target {t} outside the code"),
            VerifyErrorKind::JumpIntoInstruction(t) => {
                write!(f, "jump target {t} is inside an instruction")
            }
            VerifyErrorKind::BackwardJumpWithoutLoopEdge => {
                write!(f, "backward jump must use JumpLoop")
            }
            VerifyErrorKind::ForwardLoopEdge => write!(f, "JumpLoop must jump backward"),
            VerifyErrorKind::FallsOffEnd => write!(f, "last instruction falls through"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Checks `code` against `limits`; see the module docs for the rules.
pub fn verify(code: &[u8], limits: &Limits) -> Result<(), VerifyError> {
    if code.is_empty() {
        return Err(VerifyError {
            pc: 0,
            kind: VerifyErrorKind::Empty,
        });
    }
    let mut starts = vec![false; code.len()];
    // (pc of the jumping instruction, absolute target).
    let mut jumps: Vec<(usize, i64)> = Vec::new();
    let mut last: Option<Opcode> = None;
    for item in instructions(code) {
        let (pc, insn) = item?;
        starts[pc] = true;
        let op = insn.opcode();
        let loop_edge = op.flags().contains(Flags::LOOP_EDGE);
        let mut err = None;
        insn.for_each_operand(&mut |v| {
            if err.is_some() {
                return;
            }
            err = match v {
                OperandValue::Reg(r) if r.0 >= limits.registers => {
                    Some(VerifyErrorKind::RegisterOutOfRange(r.0))
                }
                OperandValue::Const(c) if c.0 >= limits.constants => {
                    Some(VerifyErrorKind::ConstantOutOfRange(c.0))
                }
                OperandValue::Ic(i) if i.0 >= limits.ics => {
                    Some(VerifyErrorKind::IcOutOfRange(i.0))
                }
                OperandValue::Label(l) => {
                    if loop_edge && l.0 > 0 {
                        Some(VerifyErrorKind::ForwardLoopEdge)
                    } else if !loop_edge && l.0 <= 0 {
                        Some(VerifyErrorKind::BackwardJumpWithoutLoopEdge)
                    } else {
                        jumps.push((pc, pc as i64 + l.0 as i64));
                        None
                    }
                }
                _ => None,
            };
        });
        if let Some(kind) = err {
            return Err(VerifyError { pc, kind });
        }
        last = Some(op);
    }
    let last = last.expect("non-empty code decodes at least one instruction");
    if !last.flags().ends_block() {
        // Report at the last instruction's start.
        let pc = starts.iter().rposition(|&s| s).unwrap_or(0);
        return Err(VerifyError {
            pc,
            kind: VerifyErrorKind::FallsOffEnd,
        });
    }
    for (pc, target) in jumps {
        if target < 0 || target >= code.len() as i64 {
            return Err(VerifyError {
                pc,
                kind: VerifyErrorKind::JumpOutOfBounds(target),
            });
        }
        if !starts[target as usize] {
            return Err(VerifyError {
                pc,
                kind: VerifyErrorKind::JumpIntoInstruction(target as usize),
            });
        }
    }
    Ok(())
}

/// One line per instruction: `pc: Name operands`, with jump targets also
/// shown as absolute offsets.
pub fn disassemble(code: &[u8]) -> Result<String, DecodeError> {
    let mut out = String::new();
    for item in instructions(code) {
        let (pc, insn) = item?;
        let _ = write!(out, "{pc:04}: {insn}");
        let mut target = None;
        insn.for_each_operand(&mut |v| {
            if let OperandValue::Label(l) = v {
                target = Some(pc as i64 + l.0 as i64);
            }
        });
        if let Some(t) = target {
            let _ = write!(out, "  ; -> {t:04}");
        }
        out.push('\n');
    }
    Ok(out)
}

impl fmt::Display for crate::Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.opcode().name())?;
        let mut first = true;
        let mut res = Ok(());
        self.for_each_operand(&mut |v| {
            if res.is_ok() {
                res = if first {
                    write!(f, " {v}")
                } else {
                    write!(f, ", {v}")
                };
                first = false;
            }
        });
        res
    }
}
