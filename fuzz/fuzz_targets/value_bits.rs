//! Untrusted 64-bit patterns through `Value::from_bits_checked` (D-006).
//!
//! Any accepted pattern must decode without panicking, and re-encoding what
//! it decodes to through the public constructors must give back exactly the
//! same bits. Together with `value_roundtrip` this shows the checked decoder
//! accepts exactly the encodings the constructors produce.
#![no_main]

use ferrojs_value::{Kind, Value, CANONICAL_NAN};
use libfuzzer_sys::fuzz_target;

fn encode(kind: Kind) -> Value {
    match kind {
        Kind::Undefined => Value::UNDEFINED,
        Kind::Null => Value::NULL,
        Kind::Bool(b) => Value::from_bool(b),
        Kind::Int32(i) => Value::from_i32(i),
        Kind::Double(d) => Value::from_f64(d),
        Kind::Object(r) => Value::object(r),
        Kind::String(r) => Value::string(r),
        Kind::Other(k, r) => Value::other(k, r),
    }
}

fuzz_target!(|data: [u8; 8]| {
    let bits = u64::from_le_bytes(data);
    let Some(v) = Value::from_bits_checked(bits) else {
        return;
    };
    assert_eq!(v.to_bits(), bits);
    let kind = v.kind();
    assert_eq!(
        encode(kind).to_bits(),
        bits,
        "non-canonical encoding accepted: {bits:#018x}"
    );

    // The predicates agree with the decoded kind.
    assert_eq!(v.is_double(), matches!(kind, Kind::Double(_)));
    assert_eq!(v.is_int32(), matches!(kind, Kind::Int32(_)));
    assert_eq!(v.is_undefined(), kind == Kind::Undefined);
    assert_eq!(v.is_null(), kind == Kind::Null);
    if let Kind::Double(d) = kind {
        assert!(!d.is_nan() || bits == CANONICAL_NAN);
    }
    let _ = format!("{v:?}");
});
