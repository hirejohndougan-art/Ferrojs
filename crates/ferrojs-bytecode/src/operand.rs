//! Operand kinds (D-019) and how each one is encoded.
//!
//! Every operand is made of one or two *units*. A unit is 1 byte in the
//! normal form, 2 bytes after a `Wide` prefix and 4 bytes after an
//! `ExtraWide` prefix; all units of one instruction share that width.
//! Units are unsigned except for `Imm` and `Label`, which are two's
//! complement. Multi-byte units are little-endian.

use core::fmt;

use crate::encode::{Reader, Unit};
use crate::DecodeErrorKind;

/// Operand width of one instruction, chosen by its widest unit.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Width {
    /// 1-byte units, no prefix.
    Narrow,
    /// 2-byte units, `Wide` prefix.
    Wide,
    /// 4-byte units, `ExtraWide` prefix.
    ExtraWide,
}

impl Width {
    /// Bytes per unit.
    #[inline]
    pub const fn bytes(self) -> usize {
        match self {
            Width::Narrow => 1,
            Width::Wide => 2,
            Width::ExtraWide => 4,
        }
    }

    /// The narrowest width that holds an unsigned unit.
    #[inline]
    pub const fn for_unsigned(v: u32) -> Width {
        if v <= u8::MAX as u32 {
            Width::Narrow
        } else if v <= u16::MAX as u32 {
            Width::Wide
        } else {
            Width::ExtraWide
        }
    }

    /// The narrowest width that holds a signed unit.
    #[inline]
    pub const fn for_signed(v: i32) -> Width {
        if v >= i8::MIN as i32 && v <= i8::MAX as i32 {
            Width::Narrow
        } else if v >= i16::MIN as i32 && v <= i16::MAX as i32 {
            Width::Wide
        } else {
            Width::ExtraWide
        }
    }
}

/// The operand kinds of D-019.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum OperandKind {
    /// A register index (`r0` is the first register after the frame header).
    Reg,
    /// A signed integer immediate.
    Imm,
    /// An index into the function's constant pool.
    Const,
    /// An index into the function's feedback vector.
    Ic,
    /// A signed jump offset, relative to the first byte of the jumping
    /// instruction (its prefix, if it has one).
    Label,
    /// An argument or element count.
    Count,
    /// A (depth, slot) pair addressing a captured variable; two units.
    EnvSlot,
    /// An index into the function's table of nested function templates.
    Func,
}

impl OperandKind {
    /// How many units the operand takes.
    #[inline]
    pub const fn units(self) -> usize {
        match self {
            OperandKind::EnvSlot => 2,
            _ => 1,
        }
    }
}

/// A register index.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Reg(pub u32);
/// A signed integer immediate.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Imm(pub i32);
/// A constant-pool index.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Const(pub u32);
/// A feedback-vector (inline cache) index.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Ic(pub u32);
/// A decoded jump offset, relative to the start of the jumping instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Label(pub i32);
/// An argument or element count.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Count(pub u32);
/// An index into the function's nested-function table.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Func(pub u32);
/// A captured variable: `depth` environments out, slot `slot`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct EnvSlot {
    pub depth: u32,
    pub slot: u32,
}

/// A forward or backward jump target handed out by
/// [`Assembler::new_label`](crate::Assembler::new_label) and bound with
/// [`Assembler::bind`](crate::Assembler::bind).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct AsmLabel(pub(crate) u32);

/// Any operand, for code that walks operands generically (verifier,
/// disassembler).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum OperandValue {
    Reg(Reg),
    Imm(Imm),
    Const(Const),
    Ic(Ic),
    Label(Label),
    Count(Count),
    EnvSlot(EnvSlot),
    Func(Func),
}

/// Implemented by each operand type; the op-definition macro uses it to
/// encode, decode and describe operands.
pub trait Operand: Copy {
    /// The kind, for metadata tables.
    const KIND: OperandKind;
    /// What the assembler takes for this operand (`AsmLabel` for labels).
    type Asm: Copy;
    /// Appends the operand's units for the assembler.
    fn push_asm(a: Self::Asm, out: &mut Vec<Unit>);
    /// Appends the operand's units, for encoding a decoded instruction.
    fn push_units(self, out: &mut Vec<Unit>);
    /// Reads the operand at the reader's width.
    fn read(r: &mut Reader<'_>) -> Result<Self, DecodeErrorKind>;
    /// The operand as an [`OperandValue`].
    fn value(self) -> OperandValue;
}

macro_rules! simple_operand {
    ($ty:ident, $kind:ident, unsigned) => {
        impl Operand for $ty {
            const KIND: OperandKind = OperandKind::$kind;
            type Asm = $ty;
            #[inline]
            fn push_asm(a: $ty, out: &mut Vec<Unit>) {
                out.push(Unit::Unsigned(a.0));
            }
            #[inline]
            fn push_units(self, out: &mut Vec<Unit>) {
                out.push(Unit::Unsigned(self.0));
            }
            #[inline]
            fn read(r: &mut Reader<'_>) -> Result<Self, DecodeErrorKind> {
                Ok($ty(r.unsigned()?))
            }
            #[inline]
            fn value(self) -> OperandValue {
                OperandValue::$kind(self)
            }
        }
    };
    ($ty:ident, $kind:ident, signed) => {
        impl Operand for $ty {
            const KIND: OperandKind = OperandKind::$kind;
            type Asm = $ty;
            #[inline]
            fn push_asm(a: $ty, out: &mut Vec<Unit>) {
                out.push(Unit::Signed(a.0));
            }
            #[inline]
            fn push_units(self, out: &mut Vec<Unit>) {
                out.push(Unit::Signed(self.0));
            }
            #[inline]
            fn read(r: &mut Reader<'_>) -> Result<Self, DecodeErrorKind> {
                Ok($ty(r.signed()?))
            }
            #[inline]
            fn value(self) -> OperandValue {
                OperandValue::$kind(self)
            }
        }
    };
}

simple_operand!(Reg, Reg, unsigned);
simple_operand!(Imm, Imm, signed);
simple_operand!(Const, Const, unsigned);
simple_operand!(Ic, Ic, unsigned);
simple_operand!(Count, Count, unsigned);
simple_operand!(Func, Func, unsigned);

impl Operand for Label {
    const KIND: OperandKind = OperandKind::Label;
    type Asm = AsmLabel;
    #[inline]
    fn push_asm(a: AsmLabel, out: &mut Vec<Unit>) {
        out.push(Unit::Label(a));
    }
    #[inline]
    fn push_units(self, out: &mut Vec<Unit>) {
        out.push(Unit::Signed(self.0));
    }
    #[inline]
    fn read(r: &mut Reader<'_>) -> Result<Self, DecodeErrorKind> {
        Ok(Label(r.signed()?))
    }
    #[inline]
    fn value(self) -> OperandValue {
        OperandValue::Label(self)
    }
}

impl Operand for EnvSlot {
    const KIND: OperandKind = OperandKind::EnvSlot;
    type Asm = EnvSlot;
    #[inline]
    fn push_asm(a: EnvSlot, out: &mut Vec<Unit>) {
        a.push_units(out);
    }
    #[inline]
    fn push_units(self, out: &mut Vec<Unit>) {
        out.push(Unit::Unsigned(self.depth));
        out.push(Unit::Unsigned(self.slot));
    }
    #[inline]
    fn read(r: &mut Reader<'_>) -> Result<Self, DecodeErrorKind> {
        let depth = r.unsigned()?;
        let slot = r.unsigned()?;
        Ok(EnvSlot { depth, slot })
    }
    #[inline]
    fn value(self) -> OperandValue {
        OperandValue::EnvSlot(self)
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}
impl fmt::Display for Imm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl fmt::Display for Const {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "c{}", self.0)
    }
}
impl fmt::Display for Ic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ic{}", self.0)
    }
}
impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:+}", self.0)
    }
}
impl fmt::Display for Count {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}
impl fmt::Display for Func {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn{}", self.0)
    }
}

impl fmt::Display for EnvSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "env[{}:{}]", self.depth, self.slot)
    }
}
impl fmt::Display for OperandValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OperandValue::Reg(x) => x.fmt(f),
            OperandValue::Imm(x) => x.fmt(f),
            OperandValue::Const(x) => x.fmt(f),
            OperandValue::Ic(x) => x.fmt(f),
            OperandValue::Label(x) => x.fmt(f),
            OperandValue::Count(x) => x.fmt(f),
            OperandValue::EnvSlot(x) => x.fmt(f),
            OperandValue::Func(x) => x.fmt(f),
        }
    }
}
