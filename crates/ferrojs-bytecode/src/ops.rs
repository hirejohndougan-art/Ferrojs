//! Every bytecode op, defined once (D-020). See `define.rs` for the
//! syntax and what is generated from this list.
//!
//! Opcode bytes follow definition order, and cached bytecode (later)
//! depends on them: add new ops at the end of a group only when no cache
//! format exists yet, and once one does, bump its version.
//!
//! Phase 1 starts with loads, moves, arithmetic, comparisons, jumps and
//! captured variables; task 1.6 adds what the compiler needs.

use ferrojs_value::Value;

use crate::number::{self, CompareOp, NumericOp, UnaryOp};
#[allow(unused_imports)]
use crate::{
    Completion, Const, Count, EnvSlot, ErrorKind, Feedback, Flow, Func, GlobalKind, Ic, Imm, Label,
    OpCtx, Reg, Thrown,
};

/// `int32 op int32` without overflow, else `number op number`, else `None`.
/// Records the feedback for the fast cases.
#[inline]
fn numeric<C: OpCtx + ?Sized>(
    cx: &mut C,
    ic: Ic,
    a: Value,
    b: Value,
    int: impl FnOnce(i32, i32) -> Option<i32>,
    float: impl FnOnce(f64, f64) -> f64,
) -> Option<Value> {
    if let (Some(x), Some(y)) = (a.as_int32(), b.as_int32()) {
        if let Some(r) = int(x, y) {
            cx.note(ic, Feedback::INT32);
            return Some(Value::from_i32(r));
        }
    }
    if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
        cx.note(ic, Feedback::NUMBER);
        return Some(Value::number(float(x, y)));
    }
    None
}

/// Integer multiply that leaves results JS would make `-0` to the double
/// path (`0 * -5` is `-0`, which int32 can't hold).
#[inline]
fn int_mul(x: i32, y: i32) -> Option<i32> {
    match x.checked_mul(y) {
        Some(0) if x < 0 || y < 0 => None,
        r => r,
    }
}

/// The handler body shared by every [`NumericOp`] op: int32 fast path,
/// then number, then the runtime.
#[inline]
fn numeric_op<C: OpCtx + ?Sized>(
    cx: &mut C,
    op: NumericOp,
    dst: Reg,
    lhs: Reg,
    rhs: Reg,
    ic: Ic,
) -> Flow {
    let (a, b) = (cx.reg(lhs), cx.reg(rhs));
    if let Some(r) = numeric(
        cx,
        ic,
        a,
        b,
        |x, y| number::numeric_int(op, x, y),
        |x, y| number::numeric(op, x, y),
    ) {
        cx.set(dst, r);
        return Flow::Next;
    }
    cx.note(ic, Feedback::OTHER);
    let c = cx.rt_numeric(op, a, b);
    cx.complete(dst, c)
}

/// The handler body shared by every [`CompareOp`] op.
#[inline]
fn compare_op<C: OpCtx + ?Sized>(
    cx: &mut C,
    op: CompareOp,
    dst: Reg,
    lhs: Reg,
    rhs: Reg,
    ic: Ic,
) -> Flow {
    let (a, b) = (cx.reg(lhs), cx.reg(rhs));
    if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
        let fb = if a.is_int32() && b.is_int32() {
            Feedback::INT32
        } else {
            Feedback::NUMBER
        };
        cx.note(ic, fb);
        cx.set(dst, Value::from_bool(number::compare(op, x, y)));
        return Flow::Next;
    }
    cx.note(ic, Feedback::OTHER);
    let c = cx.rt_compare(op, a, b);
    cx.complete(dst, c)
}

/// The handler body shared by every [`UnaryOp`] op.
#[inline]
fn unary_op<C: OpCtx + ?Sized>(cx: &mut C, op: UnaryOp, dst: Reg, src: Reg, ic: Ic) -> Flow {
    let a = cx.reg(src);
    if let Some(r) = a.as_int32().and_then(|x| number::unary_int(op, x)) {
        cx.note(ic, Feedback::INT32);
        cx.set(dst, Value::from_i32(r));
        return Flow::Next;
    }
    if let Some(x) = a.as_number() {
        cx.note(ic, Feedback::NUMBER);
        cx.set(dst, Value::number(number::unary(op, x)));
        return Flow::Next;
    }
    cx.note(ic, Feedback::OTHER);
    let c = cx.rt_unary(op, a);
    cx.complete(dst, c)
}

/// The op list, with room for extra ops at the end (the `padding-ops`
/// feature adds unused ones to measure dispatch against the op count,
/// D-052).
macro_rules! all_ops {
    ($($extra:tt)*) => {
define_ops! {
    /// `dst = src`.
    Mov(mov) { dst: Reg, src: Reg }
        flags()
        handler(cx) {
            let v = cx.reg(src);
            cx.set(dst, v);
            Flow::Next
        }

    /// `dst = undefined`.
    LoadUndefined(load_undefined) { dst: Reg }
        flags()
        handler(cx) {
            cx.set(dst, Value::UNDEFINED);
            Flow::Next
        }

    /// `dst = null`.
    LoadNull(load_null) { dst: Reg }
        flags()
        handler(cx) {
            cx.set(dst, Value::NULL);
            Flow::Next
        }

    /// `dst = true`.
    LoadTrue(load_true) { dst: Reg }
        flags()
        handler(cx) {
            cx.set(dst, Value::TRUE);
            Flow::Next
        }

    /// `dst = false`.
    LoadFalse(load_false) { dst: Reg }
        flags()
        handler(cx) {
            cx.set(dst, Value::FALSE);
            Flow::Next
        }

    /// `dst = value` (an int32 immediate).
    LoadInt(load_int) { dst: Reg, value: Imm }
        flags()
        handler(cx) {
            cx.set(dst, Value::from_i32(value.0));
            Flow::Next
        }

    /// `dst = constants[index]`.
    LoadConst(load_const) { dst: Reg, index: Const }
        flags()
        handler(cx) {
            let v = cx.constant(index);
            cx.set(dst, v);
            Flow::Next
        }

    /// `dst = lhs + rhs` (ApplyStringOrNumericBinaryOperator, `+`).
    Add(add) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            let (a, b) = (cx.reg(lhs), cx.reg(rhs));
            if let Some(r) = numeric(cx, ic, a, b, i32::checked_add, |x, y| x + y) {
                cx.set(dst, r);
                return Flow::Next;
            }
            cx.note(ic, Feedback::OTHER);
            let c = cx.rt_add(a, b);
            cx.complete(dst, c)
        }

    /// `dst = lhs - rhs`.
    Sub(sub) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            let (a, b) = (cx.reg(lhs), cx.reg(rhs));
            if let Some(r) = numeric(cx, ic, a, b, i32::checked_sub, |x, y| x - y) {
                cx.set(dst, r);
                return Flow::Next;
            }
            cx.note(ic, Feedback::OTHER);
            let c = cx.rt_sub(a, b);
            cx.complete(dst, c)
        }

    /// `dst = lhs * rhs`.
    Mul(mul) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            let (a, b) = (cx.reg(lhs), cx.reg(rhs));
            if let Some(r) = numeric(cx, ic, a, b, int_mul, |x, y| x * y) {
                cx.set(dst, r);
                return Flow::Next;
            }
            cx.note(ic, Feedback::OTHER);
            let c = cx.rt_mul(a, b);
            cx.complete(dst, c)
        }

    /// `dst = lhs < rhs`.
    LessThan(less_than) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            let (a, b) = (cx.reg(lhs), cx.reg(rhs));
            if let (Some(x), Some(y)) = (a.as_int32(), b.as_int32()) {
                cx.note(ic, Feedback::INT32);
                cx.set(dst, Value::from_bool(x < y));
                return Flow::Next;
            }
            if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
                cx.note(ic, Feedback::NUMBER);
                cx.set(dst, Value::from_bool(x < y));
                return Flow::Next;
            }
            cx.note(ic, Feedback::OTHER);
            let c = cx.rt_less_than(a, b);
            cx.complete(dst, c)
        }

    /// `dst = lhs === rhs` (IsStrictlyEqual).
    StrictEqual(strict_equal) { dst: Reg, lhs: Reg, rhs: Reg }
        flags()
        handler(cx) {
            let (a, b) = (cx.reg(lhs), cx.reg(rhs));
            let eq = match (a.as_number(), b.as_number()) {
                (Some(x), Some(y)) => x == y,
                _ => cx.rt_strict_equal(a, b),
            };
            cx.set(dst, Value::from_bool(eq));
            Flow::Next
        }

    /// `dst = !src`.
    Not(not) { dst: Reg, src: Reg }
        flags()
        handler(cx) {
            let v = cx.reg(src);
            let b = cx.to_boolean(v);
            cx.set(dst, Value::from_bool(!b));
            Flow::Next
        }

    /// Jumps forward to `target`.
    Jump(jump) { target: Label }
        flags(jump)
        handler(cx) {
            Flow::Jump(target)
        }

    /// Jumps forward to `target` if ToBoolean(`cond`) is true.
    JumpIfTrue(jump_if_true) { cond: Reg, target: Label }
        flags(branch)
        handler(cx) {
            let v = cx.reg(cond);
            if cx.to_boolean(v) { Flow::Jump(target) } else { Flow::Next }
        }

    /// Jumps forward to `target` if ToBoolean(`cond`) is false.
    JumpIfFalse(jump_if_false) { cond: Reg, target: Label }
        flags(branch)
        handler(cx) {
            let v = cx.reg(cond);
            if cx.to_boolean(v) { Flow::Next } else { Flow::Jump(target) }
        }

    /// Loop back-edge: checks for interrupts (D-007), then jumps backward
    /// to `target`. The only op allowed to jump backward.
    JumpLoop(jump_loop) { target: Label }
        flags(jump, loop_edge, may_throw, may_gc)
        handler(cx) {
            match cx.check_interrupt() {
                Flow::Next => Flow::Jump(target),
                other => other,
            }
        }

    /// `dst = ` the captured variable at `slot`.
    LoadEnv(load_env) { dst: Reg, slot: EnvSlot }
        flags(may_throw)
        handler(cx) {
            let c = cx.env_get(slot);
            cx.complete(dst, c)
        }

    /// The captured variable at `slot` ` = src`.
    StoreEnv(store_env) { slot: EnvSlot, src: Reg }
        flags(may_throw)
        handler(cx) {
            let v = cx.reg(src);
            match cx.env_set(slot, v) {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// Returns `value` from the function.
    Return(ret) { value: Reg }
        flags(terminator)
        handler(cx) {
            Flow::Return(cx.reg(value))
        }

    /// Throws `value`.
    Throw(throw) { value: Reg }
        flags(terminator, may_throw)
        handler(cx) {
            let v = cx.reg(value);
            cx.throw(v)
        }

    /// `dst = lhs / rhs`.
    Div(div) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::Div, dst, lhs, rhs, ic)
        }

    /// `dst = lhs % rhs`.
    Rem(rem) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::Rem, dst, lhs, rhs, ic)
        }

    /// `dst = lhs ** rhs`.
    Exp(exp) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::Exp, dst, lhs, rhs, ic)
        }

    /// `dst = lhs & rhs`.
    BitAnd(bit_and) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::BitAnd, dst, lhs, rhs, ic)
        }

    /// `dst = lhs | rhs`.
    BitOr(bit_or) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::BitOr, dst, lhs, rhs, ic)
        }

    /// `dst = lhs ^ rhs`.
    BitXor(bit_xor) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::BitXor, dst, lhs, rhs, ic)
        }

    /// `dst = lhs << rhs`.
    Shl(shl) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::Shl, dst, lhs, rhs, ic)
        }

    /// `dst = lhs >> rhs`.
    Shr(shr) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::Shr, dst, lhs, rhs, ic)
        }

    /// `dst = lhs >>> rhs`.
    UShr(ushr) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            numeric_op(cx, NumericOp::UShr, dst, lhs, rhs, ic)
        }

    /// `dst = lhs > rhs`.
    GreaterThan(greater_than) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            compare_op(cx, CompareOp::Greater, dst, lhs, rhs, ic)
        }

    /// `dst = lhs <= rhs`.
    LessEqual(less_equal) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            compare_op(cx, CompareOp::LessEq, dst, lhs, rhs, ic)
        }

    /// `dst = lhs >= rhs`.
    GreaterEqual(greater_equal) { dst: Reg, lhs: Reg, rhs: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            compare_op(cx, CompareOp::GreaterEq, dst, lhs, rhs, ic)
        }

    /// `dst = +src` (ToNumber).
    ToNumber(to_number) { dst: Reg, src: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            unary_op(cx, UnaryOp::ToNumber, dst, src, ic)
        }

    /// `dst = ToNumeric(src)` (the old value of `x++`).
    ToNumeric(to_numeric) { dst: Reg, src: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            unary_op(cx, UnaryOp::ToNumeric, dst, src, ic)
        }

    /// `dst = -src`.
    Negate(negate) { dst: Reg, src: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            unary_op(cx, UnaryOp::Negate, dst, src, ic)
        }

    /// `dst = ~src`.
    BitNot(bit_not) { dst: Reg, src: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            unary_op(cx, UnaryOp::BitNot, dst, src, ic)
        }

    /// `dst = ToNumeric(src) + 1`.
    Inc(inc) { dst: Reg, src: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            unary_op(cx, UnaryOp::Inc, dst, src, ic)
        }

    /// `dst = ToNumeric(src) - 1`.
    Dec(dec) { dst: Reg, src: Reg, ic: Ic }
        flags(may_throw, may_gc, has_ic)
        handler(cx) {
            unary_op(cx, UnaryOp::Dec, dst, src, ic)
        }

    /// `dst = lhs == rhs` (IsLooselyEqual).
    Equal(equal) { dst: Reg, lhs: Reg, rhs: Reg }
        flags(may_throw, may_gc)
        handler(cx) {
            let (a, b) = (cx.reg(lhs), cx.reg(rhs));
            if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
                cx.set(dst, Value::from_bool(x == y));
                return Flow::Next;
            }
            let c = cx.rt_loose_equal(a, b);
            cx.complete(dst, c)
        }

    /// Jumps forward to `target` unless `cond` is `undefined` or `null`
    /// (for `??` and `??=`).
    JumpIfNotNullish(jump_if_not_nullish) { cond: Reg, target: Label }
        flags(branch)
        handler(cx) {
            if cx.reg(cond).is_nullish() { Flow::Next } else { Flow::Jump(target) }
        }

    /// `dst = ` the TDZ marker (D-034): the binding in `dst` is not
    /// initialized yet.
    LoadUninitialized(load_uninitialized) { dst: Reg }
        flags()
        handler(cx) {
            cx.set(dst, Value::UNINITIALIZED);
            Flow::Next
        }

    /// Throws a `ReferenceError` if `src` holds the TDZ marker.
    CheckInitialized(check_initialized) { src: Reg }
        flags(may_throw, may_gc)
        handler(cx) {
            if cx.reg(src).is_uninitialized() {
                cx.rt_throw_error(ErrorKind::UninitializedBinding)
            } else {
                Flow::Next
            }
        }

    /// Throws the `TypeError` for an assignment to a `const` binding.
    ThrowConstAssign(throw_const_assign) {}
        flags(terminator, may_throw, may_gc)
        handler(cx) {
            cx.rt_throw_error(ErrorKind::ConstAssignment)
        }

    /// Throws the `ReferenceError` for an assignment to a call in sloppy
    /// code (`f() = 1`, D-045).
    ThrowInvalidAssignment(throw_invalid_assignment) {}
        flags(terminator, may_throw, may_gc)
        handler(cx) {
            cx.rt_throw_error(ErrorKind::InvalidAssignmentTarget)
        }

    /// `dst = ` a new closure of nested function `func` over the current
    /// environment.
    CreateClosure(create_closure) { dst: Reg, func: Func }
        flags(may_throw, may_gc)
        handler(cx) {
            let c = cx.rt_create_closure(func);
            cx.complete(dst, c)
        }

    /// `dst = ` the function being run (for a named function expression's
    /// own name).
    LoadClosure(load_closure) { dst: Reg }
        flags()
        handler(cx) {
            let v = cx.current_closure();
            cx.set(dst, v);
            Flow::Next
        }

    /// `dst = callee(args .. args + argc)` with `this`; the dispatch loop
    /// does the call.
    Call(call) { dst: Reg, callee: Reg, this: Reg, args: Reg, argc: Count }
        flags(may_throw, may_gc)
        handler(cx) {
            let _ = (dst, callee, this, args, argc);
            Flow::Call
        }

    /// `return callee(args .. args + argc)` as a proper tail call (D-021):
    /// the callee replaces this frame.
    TailCall(tail_call) { callee: Reg, this: Reg, args: Reg, argc: Count }
        flags(terminator, may_throw, may_gc)
        handler(cx) {
            let _ = (callee, this, args, argc);
            Flow::Call
        }

    /// Enters a new environment with `slots` slots.
    PushEnv(push_env) { slots: Count }
        flags(may_throw, may_gc)
        handler(cx) {
            match cx.push_env(slots) {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// Leaves the current environment.
    PopEnv(pop_env) {}
        flags(may_throw)
        handler(cx) {
            match cx.pop_env() {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// Replaces the current environment with a copy (per-iteration
    /// `let` bindings).
    CopyEnv(copy_env) {}
        flags(may_throw, may_gc)
        handler(cx) {
            match cx.copy_env() {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// `dst = ` the current environment.
    GetEnv(get_env) { dst: Reg }
        flags()
        handler(cx) {
            let v = cx.env_value();
            cx.set(dst, v);
            Flow::Next
        }

    /// Makes the environment saved in `src` current again (after a
    /// throw lands in a handler).
    SetEnv(set_env) { src: Reg }
        flags(may_throw)
        handler(cx) {
            let v = cx.reg(src);
            match cx.set_env(v) {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// `dst = this`.
    LoadThis(load_this) { dst: Reg }
        flags()
        handler(cx) {
            let v = cx.this_value();
            cx.set(dst, v);
            Flow::Next
        }

    /// `dst = ` the global name `constants[name]`.
    LoadName(load_name) { dst: Reg, name: Const }
        flags(may_throw, may_gc)
        handler(cx) {
            let n = cx.constant(name);
            let c = cx.rt_load_name(n);
            cx.complete(dst, c)
        }

    /// The global name `constants[name]` ` = src` (sloppy code).
    StoreName(store_name) { name: Const, src: Reg }
        flags(may_throw, may_gc)
        handler(cx) {
            let (n, v) = (cx.constant(name), cx.reg(src));
            match cx.rt_store_name(n, v, false) {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// The global name `constants[name]` ` = src` (strict code).
    StoreNameStrict(store_name_strict) { name: Const, src: Reg }
        flags(may_throw, may_gc)
        handler(cx) {
            let (n, v) = (cx.constant(name), cx.reg(src));
            match cx.rt_store_name(n, v, true) {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// Declares the global `constants[name]` as a `GlobalKind` (`kind`).
    DeclareGlobal(declare_global) { name: Const, kind: Imm }
        flags(may_throw, may_gc)
        handler(cx) {
            let n = cx.constant(name);
            let Some(k) = GlobalKind::from_imm(kind.0) else {
                return cx.rt_throw_error(ErrorKind::GlobalRedeclaration);
            };
            match cx.rt_declare_global(n, k) {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// Initializes the global lexical binding `constants[name]` to `src`.
    InitGlobal(init_global) { name: Const, src: Reg }
        flags(may_throw, may_gc)
        handler(cx) {
            let (n, v) = (cx.constant(name), cx.reg(src));
            match cx.rt_init_global(n, v) {
                Ok(()) => Flow::Next,
                Err(Thrown) => Flow::Throw,
            }
        }

    /// `dst = typeof src`.
    Typeof(type_of) { dst: Reg, src: Reg }
        flags()
        handler(cx) {
            let v = cx.reg(src);
            let t = cx.rt_typeof(v);
            cx.set(dst, t);
            Flow::Next
        }

    /// `dst = typeof ` the global name `constants[name]`: `"undefined"`
    /// for an undeclared name instead of a `ReferenceError`.
    TypeofName(type_of_name) { dst: Reg, name: Const }
        flags(may_throw, may_gc)
        handler(cx) {
            let n = cx.constant(name);
            let c = cx.rt_typeof_name(n);
            cx.complete(dst, c)
        }

    /// `dst = ToString(src)` (template literal substitutions).
    ToString(to_string) { dst: Reg, src: Reg }
        flags(may_throw, may_gc)
        handler(cx) {
            let v = cx.reg(src);
            if v.is_string() {
                cx.set(dst, v);
                return Flow::Next;
            }
            let c = cx.rt_to_string(v);
            cx.complete(dst, c)
        }

    $($extra)*
}
    };
}

#[cfg(not(feature = "padding-ops"))]
all_ops! {}

// 150 ops that are never emitted: only for measuring that dispatch cost
// doesn't depend on the number of ops (D-052).
#[cfg(feature = "padding-ops")]
include!("padding_ops.rs");
