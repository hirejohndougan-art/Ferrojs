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

use crate::number::{CompareOp, NumericOp, UnaryOp};
use crate::operand::{Const, Count, EnvSlot, Func, Ic, Label, Reg};

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
    /// Call a function: the dispatch loop reads the instruction's
    /// operands ([`CallSite::of`]), pushes (or, for a tail call, replaces)
    /// the frame and continues in the callee (ARCHITECTURE.md §3.3–3.4).
    /// Handlers never recurse into the callee themselves. It carries no
    /// data so `Flow` stays small on the hot path.
    Call,
    /// A property read or write met an accessor (task 1.8e, D-058): the
    /// runtime function left the getter or setter call in the context,
    /// and the dispatch loop makes it as an ordinary call from this
    /// instruction. A getter's result goes to the instruction's `dst`
    /// ([`accessor_dst`]); a setter's is dropped. Either way execution
    /// continues after the instruction, and a throw unwinds from it.
    Accessor,
}

/// Where the result of an accessor call made for `insn` goes: the `dst`
/// of a property read, nothing for a write.
pub fn accessor_dst(insn: &crate::Instruction) -> Option<Reg> {
    use crate::Instruction as I;
    match *insn {
        I::GetNamed(o) => Some(o.dst),
        I::GetKeyed(o) => Some(o.dst),
        I::SetNamed(_) | I::SetNamedStrict(_) | I::SetKeyed(_) | I::SetKeyedStrict(_) => None,
        // A new op that can return `Flow::Accessor` must say where the
        // result goes.
        _ => unreachable!("only property ops call accessors"),
    }
}

/// Why a property read or write didn't finish in the runtime function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Detour {
    /// An exception is pending in the context.
    Thrown,
    /// A getter or setter call is pending in the context
    /// ([`Flow::Accessor`]).
    Call,
}

impl From<Thrown> for Detour {
    fn from(_: Thrown) -> Detour {
        Detour::Thrown
    }
}

impl Detour {
    /// The flow for a handler that met this.
    #[inline]
    pub fn flow(self) -> Flow {
        match self {
            Detour::Thrown => Flow::Throw,
            Detour::Call => Flow::Accessor,
        }
    }
}

/// The result of a property read: the value, or a detour.
pub type Access = Result<Value, Detour>;

/// The operands of a `Call` or `TailCall`, for the dispatch loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CallSite {
    /// Where the result goes (unused for tail calls).
    pub dst: Reg,
    pub callee: Reg,
    pub this: Reg,
    /// First of `argc` consecutive argument registers.
    pub args: Reg,
    pub argc: Count,
    /// A proper tail call: the callee replaces the current frame.
    pub tail: bool,
}

impl CallSite {
    /// The call operands of `insn`, if it is a `Call` or `TailCall`.
    pub fn of(insn: &crate::Instruction) -> Option<CallSite> {
        match *insn {
            crate::Instruction::Call(o) => Some(CallSite {
                dst: o.dst,
                callee: o.callee,
                this: o.this,
                args: o.args,
                argc: o.argc,
                tail: false,
            }),
            crate::Instruction::TailCall(o) => Some(CallSite {
                dst: Reg(0),
                callee: o.callee,
                this: o.this,
                args: o.args,
                argc: o.argc,
                tail: true,
            }),
            _ => None,
        }
    }
}

/// Marker: an exception was thrown and is pending in the context.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Thrown;

/// Errors op handlers ask the runtime to throw.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ErrorKind {
    /// `ReferenceError`: a binding was used in its temporal dead zone.
    UninitializedBinding,
    /// `TypeError`: assignment to a `const` binding.
    ConstAssignment,
    /// `TypeError`: the callee is not a function.
    NotCallable,
    /// `RangeError`: too much recursion.
    StackOverflow,
    /// `ReferenceError`: a name that isn't declared anywhere.
    UnresolvableReference,
    /// `SyntaxError`: a global declaration clashes with an existing one
    /// (GlobalDeclarationInstantiation).
    GlobalRedeclaration,
    /// `ReferenceError`: sloppy code assigned to a call, like `f() = 1`
    /// (assignment target type *web-compat*, D-045).
    InvalidAssignmentTarget,
    /// `TypeError`: a property of `undefined` or `null` was read,
    /// written or deleted (task 1.7e).
    NullishBase,
    /// `TypeError`: the right side of `in` is not an object.
    NotAnObject,
    /// `TypeError`: strict code assigned or deleted a property and the
    /// object refused (a non-writable or non-configurable property, a
    /// primitive base).
    PropertyRefused,
    /// `TypeError`: a host function returned or threw an object or a
    /// function, which can't be made from the host side yet (D-054).
    HostResult,
}

/// What a global declaration creates (GlobalDeclarationInstantiation).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum GlobalKind {
    /// `var` and function declarations: a property of the global object.
    Var,
    /// `let`: a binding of the global lexical environment, in its TDZ
    /// until initialized.
    Let,
    /// `const`: like `let`, immutable once initialized.
    Const,
}

impl GlobalKind {
    /// Decodes the `Imm` operand of `DeclareGlobal` (unknown values are
    /// `None`; the handler throws for them).
    pub fn from_imm(i: i32) -> Option<GlobalKind> {
        match i {
            0 => Some(GlobalKind::Var),
            1 => Some(GlobalKind::Let),
            2 => Some(GlobalKind::Const),
            _ => None,
        }
    }

    pub fn imm(self) -> i32 {
        match self {
            GlobalKind::Var => 0,
            GlobalKind::Let => 1,
            GlobalKind::Const => 2,
        }
    }
}

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

    /// Creates a closure of nested function `f` over the current
    /// environment.
    fn rt_create_closure(&mut self, f: Func) -> Completion;
    /// The function object being run (the frame's callee).
    fn current_closure(&self) -> Value;
    /// Makes a new environment with `slots` slots (each holding
    /// `undefined`) whose parent is the current one, and enters it.
    fn push_env(&mut self, slots: Count) -> Result<(), Thrown>;
    /// Leaves the current environment for its parent.
    fn pop_env(&mut self) -> Result<(), Thrown>;
    /// Replaces the current environment with a copy of it (same parent,
    /// same slot values): CreatePerIterationEnvironment.
    fn copy_env(&mut self) -> Result<(), Thrown>;
    /// The current environment as a value (to save in a register).
    fn env_value(&self) -> Value;
    /// Makes `v`, a value from [`env_value`](Self::env_value), the current
    /// environment again.
    fn set_env(&mut self, v: Value) -> Result<(), Thrown>;

    /// Reads a captured variable (may throw: TDZ, or a bad slot).
    fn env_get(&mut self, slot: EnvSlot) -> Completion;
    /// Writes a captured variable (may throw: TDZ, const assignment).
    fn env_set(&mut self, slot: EnvSlot, v: Value) -> Result<(), Thrown>;

    // Named runtime functions: the slow paths every tier calls.

    /// Creates and throws the error `e` describes; returns [`Flow::Throw`].
    fn rt_throw_error(&mut self, e: ErrorKind) -> Flow;

    /// The frame's `this`.
    fn this_value(&self) -> Value;
    /// Reads the global name `name` (a string constant): a global lexical
    /// binding, else a property of the global object; `ReferenceError` if
    /// neither exists or the binding is in its TDZ.
    fn rt_load_name(&mut self, name: Value) -> Completion;
    /// Assigns the global name `name`. An undeclared name becomes a
    /// global property in sloppy code and is a `ReferenceError` in strict
    /// code; a TDZ binding is a `ReferenceError`; a `const` a `TypeError`.
    fn rt_store_name(&mut self, name: Value, v: Value, strict: bool) -> Result<(), Thrown>;
    /// Declares a global (GlobalDeclarationInstantiation for one name):
    /// `SyntaxError` on a clash between lexical and other declarations.
    fn rt_declare_global(&mut self, name: Value, kind: GlobalKind) -> Result<(), Thrown>;
    /// Initializes a global lexical binding (ends its TDZ).
    fn rt_init_global(&mut self, name: Value, v: Value) -> Result<(), Thrown>;

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
    /// A [`NumericOp`] (`/ % ** & | ^ << >> >>>`) for operands the fast
    /// path didn't handle: ToNumeric on both (left first), then the op.
    fn rt_numeric(&mut self, op: NumericOp, a: Value, b: Value) -> Completion;
    /// A [`CompareOp`] (`> <= >=`) for operands that aren't both numbers.
    fn rt_compare(&mut self, op: CompareOp, a: Value, b: Value) -> Completion;
    /// A [`UnaryOp`] for an operand that isn't a number.
    fn rt_unary(&mut self, op: UnaryOp, a: Value) -> Completion;
    /// `a == b` (IsLooselyEqual) for operands that aren't both numbers.
    fn rt_loose_equal(&mut self, a: Value, b: Value) -> Completion;
    /// ToBoolean for values that aren't booleans. Must not allocate or run
    /// user code.
    fn rt_to_boolean(&mut self, v: Value) -> bool;
    /// `typeof v`: one of the eight type-name strings. Must not allocate
    /// (the names are pre-made atoms) or run user code.
    fn rt_typeof(&mut self, v: Value) -> Value;
    /// `typeof name` for a global name `name` (a string constant): like
    /// [`rt_load_name`](Self::rt_load_name), except that an unresolvable
    /// name gives `"undefined"` instead of throwing. A binding in its TDZ
    /// still throws.
    fn rt_typeof_name(&mut self, name: Value) -> Completion;
    /// ToString for a value that isn't a string (template substitutions).
    fn rt_to_string(&mut self, v: Value) -> Completion;

    // Properties (task 1.7e). Keys are any primitive (ToPropertyKey runs
    // inside); named ops pass their string constant.

    /// A new ordinary object (an object literal).
    fn rt_new_object(&mut self) -> Completion;
    /// `obj[key]` (GetValue of a property reference): a `TypeError` for an
    /// `undefined` or `null` base. A getter is left in the context to be
    /// called with `obj` as `this` ([`Detour::Call`]).
    fn rt_get_property(&mut self, obj: Value, key: Value) -> Access;
    /// `obj[key] = v` (PutValue): a `TypeError` for an `undefined` or
    /// `null` base; in strict code also when the assignment is refused. A
    /// setter is left in the context to be called with `obj` as `this` and
    /// `v` as its argument ([`Detour::Call`]).
    fn rt_set_property(
        &mut self,
        obj: Value,
        key: Value,
        v: Value,
        strict: bool,
    ) -> Result<(), Detour>;
    /// CreateDataPropertyOrThrow on `obj` (object literal members).
    fn rt_define_property(&mut self, obj: Value, key: Value, v: Value) -> Result<(), Thrown>;
    /// Defines the getter (`setter` false) or setter half of accessor
    /// property `key` of `obj` as function `f`, enumerable and
    /// configurable (an object literal's `get` or `set` member:
    /// DefinePropertyOrThrow with only that half present).
    fn rt_define_accessor(
        &mut self,
        obj: Value,
        key: Value,
        f: Value,
        setter: bool,
    ) -> Result<(), Thrown>;
    /// `delete obj[key]`: whether the property is gone; in strict code a
    /// refusal is a `TypeError`.
    fn rt_delete_property(&mut self, obj: Value, key: Value, strict: bool) -> Completion;
    /// `key in obj` (HasProperty): a `TypeError` unless `obj` is an
    /// object.
    fn rt_has_property(&mut self, key: Value, obj: Value) -> Completion;

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
