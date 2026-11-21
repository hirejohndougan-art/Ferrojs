//! Number semantics shared by every tier (ECMA-262 §6.1.6.1): the fast
//! paths in op handlers and the runtime slow paths both call these, so the
//! interpreter, the JIT and the toy context can't disagree on corner cases.

/// Binary operators with a numeric fast path that share one slow path,
/// [`OpCtx::rt_numeric`](crate::OpCtx::rt_numeric).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum NumericOp {
    Div,
    Rem,
    Exp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
}

/// Relational operators other than `<`, sharing
/// [`OpCtx::rt_compare`](crate::OpCtx::rt_compare).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CompareOp {
    /// `a > b`: IsLessThan(b, a, LeftFirst = false).
    Greater,
    /// `a <= b`: !IsLessThan(b, a, LeftFirst = false), undefined → false.
    LessEq,
    /// `a >= b`: !IsLessThan(a, b, LeftFirst = true), undefined → false.
    GreaterEq,
}

/// Unary numeric operators sharing
/// [`OpCtx::rt_unary`](crate::OpCtx::rt_unary).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum UnaryOp {
    /// Unary `+`: ToNumber.
    ToNumber,
    /// ToNumeric (the old value of `x++`).
    ToNumeric,
    /// Unary `-`.
    Negate,
    /// `~`.
    BitNot,
    /// ToNumeric, then `+ 1`.
    Inc,
    /// ToNumeric, then `- 1`.
    Dec,
}

/// ToInt32 (§7.1.6).
pub fn to_int32(d: f64) -> i32 {
    to_uint32(d) as i32
}

/// ToUint32 (§7.1.7).
pub fn to_uint32(d: f64) -> u32 {
    if !d.is_finite() {
        return 0;
    }
    let t = d.trunc();
    // Modulo 2^32 on the exact integer value; rem_euclid keeps it exact
    // because both operands are integers representable in f64.
    t.rem_euclid(4_294_967_296.0) as u32
}

/// Number::exponentiate (§6.1.6.1.3). Differs from `powf` where the base's
/// magnitude is 1 and the exponent is infinite or NaN.
pub fn exponentiate(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() {
        return f64::NAN;
    }
    if exponent == 0.0 {
        return 1.0;
    }
    if base.abs() == 1.0 && exponent.is_infinite() {
        return f64::NAN;
    }
    base.powf(exponent)
}

/// A [`NumericOp`] on two numbers.
pub fn numeric(op: NumericOp, a: f64, b: f64) -> f64 {
    match op {
        NumericOp::Div => a / b,
        // Rust's `%` on f64 is C's fmod, which is Number::remainder.
        NumericOp::Rem => a % b,
        NumericOp::Exp => exponentiate(a, b),
        NumericOp::BitAnd => f64::from(to_int32(a) & to_int32(b)),
        NumericOp::BitOr => f64::from(to_int32(a) | to_int32(b)),
        NumericOp::BitXor => f64::from(to_int32(a) ^ to_int32(b)),
        NumericOp::Shl => f64::from(to_int32(a).wrapping_shl(to_uint32(b) & 31)),
        NumericOp::Shr => f64::from(to_int32(a).wrapping_shr(to_uint32(b) & 31)),
        NumericOp::UShr => f64::from(to_uint32(a).wrapping_shr(to_uint32(b) & 31)),
    }
}

/// A [`NumericOp`] on two int32s, when the result is an int32 too (`None`
/// sends the operation to the double path).
pub fn numeric_int(op: NumericOp, a: i32, b: i32) -> Option<i32> {
    match op {
        NumericOp::BitAnd => Some(a & b),
        NumericOp::BitOr => Some(a | b),
        NumericOp::BitXor => Some(a ^ b),
        NumericOp::Shl => Some(a.wrapping_shl(b as u32 & 31)),
        NumericOp::Shr => Some(a.wrapping_shr(b as u32 & 31)),
        NumericOp::UShr => i32::try_from((a as u32).wrapping_shr(b as u32 & 31)).ok(),
        // `-7 % 7` is -0, which int32 can't hold.
        NumericOp::Rem => match a.checked_rem(b) {
            Some(0) if a < 0 => None,
            r => r,
        },
        NumericOp::Div | NumericOp::Exp => None,
    }
}

/// A [`CompareOp`] on two numbers (NaN compares false every way).
pub fn compare(op: CompareOp, a: f64, b: f64) -> bool {
    match op {
        CompareOp::Greater => a > b,
        CompareOp::LessEq => a <= b,
        CompareOp::GreaterEq => a >= b,
    }
}

/// A [`UnaryOp`] on a number.
pub fn unary(op: UnaryOp, a: f64) -> f64 {
    match op {
        UnaryOp::ToNumber | UnaryOp::ToNumeric => a,
        UnaryOp::Negate => -a,
        UnaryOp::BitNot => f64::from(!to_int32(a)),
        UnaryOp::Inc => a + 1.0,
        UnaryOp::Dec => a - 1.0,
    }
}

/// A [`UnaryOp`] on an int32, when the result is an int32 too.
pub fn unary_int(op: UnaryOp, a: i32) -> Option<i32> {
    match op {
        UnaryOp::ToNumber | UnaryOp::ToNumeric => Some(a),
        // `-0` isn't an int32.
        UnaryOp::Negate if a == 0 => None,
        UnaryOp::Negate => a.checked_neg(),
        UnaryOp::BitNot => Some(!a),
        UnaryOp::Inc => a.checked_add(1),
        UnaryOp::Dec => a.checked_sub(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int32_conversions() {
        assert_eq!(to_int32(f64::NAN), 0);
        assert_eq!(to_int32(f64::INFINITY), 0);
        assert_eq!(to_int32(-0.0), 0);
        assert_eq!(to_int32(2_147_483_648.0), i32::MIN);
        assert_eq!(to_int32(4_294_967_297.5), 1);
        assert_eq!(to_int32(-1.9), -1);
        assert_eq!(to_uint32(-1.0), u32::MAX);
        assert_eq!(
            to_int32(1e21),
            (1e21f64.rem_euclid(4_294_967_296.0)) as u32 as i32
        );
    }

    #[test]
    fn exponent_corner_cases() {
        assert!(exponentiate(1.0, f64::NAN).is_nan());
        assert!(exponentiate(-1.0, f64::INFINITY).is_nan());
        assert_eq!(exponentiate(f64::NAN, 0.0), 1.0);
        assert_eq!(exponentiate(2.0, 10.0), 1024.0);
    }

    #[test]
    fn int_paths_agree_with_double_paths() {
        let ops = [
            NumericOp::Div,
            NumericOp::Rem,
            NumericOp::Exp,
            NumericOp::BitAnd,
            NumericOp::BitOr,
            NumericOp::BitXor,
            NumericOp::Shl,
            NumericOp::Shr,
            NumericOp::UShr,
        ];
        let vals = [0, 1, -1, 7, -7, 31, 32, 33, i32::MAX, i32::MIN, 1000];
        for op in ops {
            for &a in &vals {
                for &b in &vals {
                    if let Some(r) = numeric_int(op, a, b) {
                        let d = numeric(op, f64::from(a), f64::from(b));
                        assert_eq!(f64::from(r).to_bits(), d.to_bits(), "{op:?} {a} {b}");
                    }
                }
            }
        }
        for op in [
            UnaryOp::Negate,
            UnaryOp::BitNot,
            UnaryOp::Inc,
            UnaryOp::Dec,
            UnaryOp::ToNumber,
        ] {
            for &a in &vals {
                if let Some(r) = unary_int(op, a) {
                    assert_eq!(f64::from(r).to_bits(), unary(op, f64::from(a)).to_bits());
                }
            }
        }
        assert_eq!(numeric_int(NumericOp::Rem, -7, 7), None, "-0");
        assert_eq!(numeric_int(NumericOp::UShr, -1, 0), None, "2^32 - 1");
        assert_eq!(unary_int(UnaryOp::Negate, 0), None, "-0");
    }
}
