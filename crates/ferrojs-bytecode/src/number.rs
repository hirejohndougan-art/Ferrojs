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

/// Number::toString(x) in radix 10 (ECMA-262 §6.1.6.1.20): the shortest
/// digits that round-trip, laid out as the spec says.
pub fn to_js_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x < 0.0 {
        return format!("-{}", to_js_string(-x));
    }
    if x.is_infinite() {
        return "Infinity".into();
    }
    // Rust's `{:e}` prints the shortest round-trip digits: "d.ddde±x".
    let e = format!("{x:e}");
    let (mantissa, exp) = e.split_once('e').expect("exponent form");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().expect("an exponent") + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        if k == 1 {
            format!("{digits}e{sign}{}", e.abs())
        } else {
            format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs())
        }
    }
}

/// WhiteSpace or LineTerminator (ECMA-262 §12.2, §12.3), what
/// StringToNumber trims.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'
            | '\u{b}'
            | '\u{c}'
            | ' '
            | '\u{a0}'
            | '\u{feff}'
            | '\n'
            | '\r'
            | '\u{2028}'
            | '\u{2029}'
    ) || (c.is_whitespace() && !c.is_ascii())
}

/// StringToNumber (ECMA-262 §7.1.4.1.1): NaN unless the whole string,
/// whitespace trimmed, is a StringNumericLiteral.
pub fn string_to_number(text: &str) -> f64 {
    let t = text.trim_matches(is_js_whitespace);
    if t.is_empty() {
        return 0.0;
    }
    let radix = |p: &str, r: u32| -> f64 {
        if p.is_empty() || !p.chars().all(|c| c.is_digit(r)) {
            return f64::NAN;
        }
        // Exact for anything that fits in 53 bits; beyond, accumulate in
        // f64 (round-to-nearest per digit is close; exactness for huge
        // non-decimal literals is a Phase 2 item).
        p.chars().fold(0.0, |acc, c| {
            acc * f64::from(r) + f64::from(c.to_digit(r).unwrap())
        })
    };
    let lower = t.get(..2).map(str::to_ascii_lowercase);
    match lower.as_deref() {
        Some("0x") => return radix(&t[2..], 16),
        Some("0o") => return radix(&t[2..], 8),
        Some("0b") => return radix(&t[2..], 2),
        _ => {}
    }
    let (sign, body) = match t.as_bytes()[0] {
        b'+' => (1.0, &t[1..]),
        b'-' => (-1.0, &t[1..]),
        _ => (1.0, t),
    };
    if body == "Infinity" {
        return sign * f64::INFINITY;
    }
    // StrUnsignedDecimalLiteral: digits [. digits] [e [+-] digits], with
    // at least one digit before the exponent; no "inf", "nan", "_".
    let b = body.as_bytes();
    let mut i = 0;
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut mantissa_digits = i - int_start;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        mantissa_digits += i - frac_start;
    }
    if mantissa_digits == 0 {
        return f64::NAN;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return f64::NAN;
        }
    }
    if i != b.len() {
        return f64::NAN;
    }
    sign * body.parse::<f64>().unwrap_or(f64::NAN)
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
    fn number_to_string_matches_the_spec_layouts() {
        for (x, want) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (123456789.0, "123456789"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123e-20, "1.23e-18"),
            (0.000001, "0.000001"),
            (0.0000001, "1e-7"),
            (1.0 / 3.0, "0.3333333333333333"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
            (2f64.powi(53), "9007199254740992"),
        ] {
            assert_eq!(to_js_string(x), want, "{x}");
        }
    }

    #[test]
    fn string_to_number_follows_the_grammar() {
        for (s, want) in [
            ("", 0.0),
            ("  \n\t ", 0.0),
            ("12", 12.0),
            (" 12 ", 12.0),
            ("-12.5", -12.5),
            ("+.5", 0.5),
            ("5.", 5.0),
            ("1e3", 1000.0),
            ("1E-2", 0.01),
            ("0x10", 16.0),
            ("0XfF", 255.0),
            ("0o17", 15.0),
            ("0b101", 5.0),
            ("Infinity", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("\u{a0}7\u{feff}", 7.0),
        ] {
            assert_eq!(string_to_number(s), want, "{s:?}");
        }
        for s in [
            "abc", "1a", ".", "e5", "1e", "-0x10", "0x", "inf", "nan", "NaN", "1_000", "infinity",
            "1 2", "0b2",
        ] {
            assert!(string_to_number(s).is_nan(), "{s:?}");
        }
        assert!(string_to_number("-0").is_sign_negative());
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

/// The largest array index, 2^32 − 2 (ECMA-262 §6.1.7).
pub const MAX_ARRAY_INDEX: u32 = u32::MAX - 1;

/// The array index a number names as a property key: ToString(`d`) is a
/// canonical array index exactly when `d` is an integer in
/// `0 ..= 2^32 − 2` (`-0` included: its string is `"0"`).
pub fn array_index_of_number(d: f64) -> Option<u32> {
    (d.fract() == 0.0 && (0.0..=f64::from(MAX_ARRAY_INDEX)).contains(&d)).then_some(d as u32)
}

/// The array index a string names as a property key: decimal digits with
/// no leading zero (except `"0"` itself), at most 2^32 − 2.
pub fn array_index_of_units(units: &[u16]) -> Option<u32> {
    if units.is_empty() || units.len() > 10 || (units.len() > 1 && units[0] == u16::from(b'0')) {
        return None;
    }
    let mut n: u64 = 0;
    for &u in units {
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&u) {
            return None;
        }
        n = n * 10 + u64::from(u - u16::from(b'0'));
    }
    u32::try_from(n).ok().filter(|&i| i <= MAX_ARRAY_INDEX)
}

#[cfg(test)]
mod index_tests {
    use super::*;

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn array_indices_from_strings() {
        for (s, want) in [
            ("0", Some(0)),
            ("7", Some(7)),
            ("4294967294", Some(MAX_ARRAY_INDEX)),
            ("4294967295", None),
            ("99999999999", None),
            ("01", None),
            ("", None),
            ("-1", None),
            ("1.5", None),
            ("1e3", None),
            (" 1", None),
            ("٣", None),
        ] {
            assert_eq!(array_index_of_units(&units(s)), want, "{s:?}");
        }
    }

    #[test]
    fn array_indices_from_numbers() {
        for (d, want) in [
            (0.0, Some(0)),
            (-0.0, Some(0)),
            (3.0, Some(3)),
            (4294967294.0, Some(MAX_ARRAY_INDEX)),
            (4294967295.0, None),
            (-1.0, None),
            (1.5, None),
            (f64::NAN, None),
            (f64::INFINITY, None),
        ] {
            assert_eq!(array_index_of_number(d), want, "{d}");
        }
    }
}

/// The property names of `Object.prototype` (ECMA-262 §20.1.3, Annex B
/// §B.2.2). Until the built-ins exist (1.13) objects have no prototype,
/// so a read or `in` of one of these that isn't an own property, and any
/// assignment to `__proto__`, is a clean failure rather than a wrong
/// answer (D-053).
pub const OBJECT_PROTOTYPE_NAMES: [&str; 12] = [
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
    "__proto__",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
];

/// Whether `units` spell one of [`OBJECT_PROTOTYPE_NAMES`].
pub fn is_object_prototype_name(units: &[u16]) -> bool {
    OBJECT_PROTOTYPE_NAMES
        .iter()
        .any(|n| n.encode_utf16().eq(units.iter().copied()))
}
