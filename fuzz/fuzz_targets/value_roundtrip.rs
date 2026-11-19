//! Values built through the public constructors survive the trip through
//! raw bits and back, and `Value::number` keeps its numeric meaning.
#![no_main]

use arbitrary::Arbitrary;
use ferrojs_value::{HeapRef, Kind, OtherKind, Value, CANONICAL_NAN};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
enum Input {
    Undefined,
    Null,
    Bool(bool),
    Int32(i32),
    F64(u64),
    Number(u64),
    Object(u32),
    String(u32),
    Symbol(u32),
    BigInt(u32),
}

fuzz_target!(|input: Input| {
    let (v, expected) = match input {
        Input::Undefined => (Value::UNDEFINED, Some(Kind::Undefined)),
        Input::Null => (Value::NULL, Some(Kind::Null)),
        Input::Bool(b) => (Value::from_bool(b), Some(Kind::Bool(b))),
        Input::Int32(i) => (Value::from_i32(i), Some(Kind::Int32(i))),
        Input::F64(bits) => {
            let d = f64::from_bits(bits);
            let v = Value::from_f64(d);
            check_number(v, d);
            assert!(v.is_double());
            (v, None)
        }
        Input::Number(bits) => {
            let d = f64::from_bits(bits);
            let v = Value::number(d);
            check_number(v, d);
            let int_form = d.fract() == 0.0
                && d >= i32::MIN as f64
                && d <= i32::MAX as f64
                && !(d == 0.0 && d.is_sign_negative());
            assert_eq!(v.is_int32(), int_form, "number({d:?}) chose the wrong form");
            (v, None)
        }
        Input::Object(o) => (Value::object(HeapRef(o)), Some(Kind::Object(HeapRef(o)))),
        Input::String(o) => (Value::string(HeapRef(o)), Some(Kind::String(HeapRef(o)))),
        Input::Symbol(o) => (
            Value::other(OtherKind::Symbol, HeapRef(o)),
            Some(Kind::Other(OtherKind::Symbol, HeapRef(o))),
        ),
        Input::BigInt(o) => (
            Value::other(OtherKind::BigInt, HeapRef(o)),
            Some(Kind::Other(OtherKind::BigInt, HeapRef(o))),
        ),
    };
    if let Some(k) = expected {
        assert_eq!(v.kind(), k);
    }
    let back = Value::from_bits_checked(v.to_bits());
    assert_eq!(back, Some(v), "constructor output rejected: {v:?}");
});

/// `v` holds the number `d`: same value, same sign of zero, NaN canonical.
fn check_number(v: Value, d: f64) {
    let got = v.as_number().expect("a number");
    if d.is_nan() {
        assert!(got.is_nan());
        if v.is_double() {
            assert_eq!(v.to_bits(), CANONICAL_NAN);
        }
    } else {
        assert_eq!(got, d);
        assert_eq!(got.is_sign_negative(), d.is_sign_negative());
    }
}
