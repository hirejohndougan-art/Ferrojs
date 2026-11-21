//! What op handlers run against (D-020, D-031).
//!
//! Handlers see the machine only through [`OpCtx`]: register reads and
//! writes, constants, feedback, environments, and the named runtime
//! functions (`rt_*`) that hold every slow path. The interpreter
//! (`ferrojs-vm`, task 1.7) implements it over real frames; Phase 3's
//! stencil context implements it with patchable holes; tests use
//! [`testing::ToyCtx`](crate::testing::ToyCtx).

use core::fmt;

use ferrojs_value::Value;

use crate::operand::{Const, EnvSlot, Ic, Label, Reg};

/// What the dispatch loop does after a handler.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    /// Continue with the next instruction.
    Next,
    /// Continue at this offset from the start of the current instruction.
    Jump(Label),
    /// Return this value from the function.
    Return(Value),
    /// An exception is pending in the context; unwind.
    Throw,
}

/// Marker: an exception was thrown and is pending in the context.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Thrown;

/// The result of a runtime function that may throw.
pub type Completion = Result<Value, Thrown>;

/// Type feedback recorded into an IC slot. Bits only accumulate.
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Feedback(pub u8);

impl Feedback {
    pub const NONE: Feedback = Feedback(0);
    /// Both operands and the result were int32.
    pub const INT32: Feedback = Feedback(1 << 0);
    /// Numbers (some double, or an int32 result overflowed).
    pub const NUMBER: Feedback = Feedback(1 << 1);
    /// Anything that needed the runtime slow path.
    pub const OTHER: Feedback = Feedback(1 << 2);

    #[inline]
    pub const fn union(self, other: Feedback) -> Feedback {
        Feedback(self.0 | other.0)
    }

    #[inline]
    pub const fn contains(self, other: Feedback) -> bool {
        self.0 & other.0 == other.0
    }
}

impl fmt::Debug for Feedback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Feedback({:#05b})", self.0)
    }
}

/// The machine as op handlers see it.
///
/// Register, constant and IC indices are in range for verified bytecode
/// (see [`verify`](crate::verify)); implementations may still check them.
pub trait OpCtx {
    /// Reads a register.
    fn reg(&self, r: Reg) -> Value;
    /// Writes a register.
    fn set(&mut self, r: Reg, v: Value);
    /// Reads the constant pool.
    fn constant(&self, c: Const) -> Value;
    /// Records type feedback into an IC slot.
    fn note(&mut self, ic: Ic, fb: Feedback);
    /// Makes `v` the pending exception and returns [`Flow::Throw`].
    fn throw(&mut self, v: Value) -> Flow;
    /// Checks the interrupt flag (D-007): [`Flow::Next`] to go on, or
    /// [`Flow::Throw`] with a termination pending.
    fn check_interrupt(&mut self) -> Flow;

    /// Reads a captured variable (may throw: TDZ, or a bad slot).
    fn env_get(&mut self, slot: EnvSlot) -> Completion;
    /// Writes a captured variable (may throw: TDZ, const assignment).
    fn env_set(&mut self, slot: EnvSlot, v: Value) -> Result<(), Thrown>;

    // Named runtime functions: the slow paths every tier calls.

    /// `a + b` for operands the fast path didn't handle.
    fn rt_add(&mut self, a: Value, b: Value) -> Completion;
    /// `a - b` for operands the fast path didn't handle.
    fn rt_sub(&mut self, a: Value, b: Value) -> Completion;
    /// `a * b` for operands the fast path didn't handle.
    fn rt_mul(&mut self, a: Value, b: Value) -> Completion;
    /// `a < b` (IsLessThan with LeftFirst) for non-number operands.
    fn rt_less_than(&mut self, a: Value, b: Value) -> Completion;
    /// `a === b` (IsStrictlyEqual) for operands that aren't both numbers.
    /// Must not allocate or run user code.
    fn rt_strict_equal(&mut self, a: Value, b: Value) -> bool;
    /// ToBoolean for values that aren't booleans. Must not allocate or run
    /// user code.
    fn rt_to_boolean(&mut self, v: Value) -> bool;

    /// Stores a completion's value in `dst`, or reports the throw.
    #[inline]
    fn complete(&mut self, dst: Reg, c: Completion) -> Flow {
        match c {
            Ok(v) => {
                self.set(dst, v);
                Flow::Next
            }
            Err(Thrown) => Flow::Throw,
        }
    }

    /// ToBoolean with the boolean fast path inline.
    #[inline]
    fn to_boolean(&mut self, v: Value) -> bool {
        match v.as_bool() {
            Some(b) => b,
            None => self.rt_to_boolean(v),
        }
    }
}
