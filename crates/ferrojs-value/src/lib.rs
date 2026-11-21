//! NaN-boxed JavaScript values (decision D-001).
//!
//! Every JavaScript value fits in 64 bits. Doubles are stored as themselves.
//! Everything else hides inside the space of NaN bit patterns that a real
//! double never uses once NaNs are canonicalized.
//!
//! Layout (top 16 bits decide the kind):
//!
//! ```text
//! 0x0000..=0xFFF7  any double (NaN always canonicalized to 0x7FF8_0000_0000_0000)
//! 0xFFF9           undefined
//! 0xFFFA           null
//! 0xFFFB           boolean        payload bit 0
//! 0xFFFC           int32          payload low 32 bits
//! 0xFFFD           object ref     payload low 32 bits = heap offset (D-002)
//! 0xFFFE           string ref     payload low 32 bits = heap offset
//! 0xFFFF           other heap ref bits 32..47 = kind (symbol, bigint), low 32 = offset
//! ```
//!
//! Heap references are 32-bit offsets into the engine instance's heap cage,
//! never raw pointers, so this crate is entirely safe Rust.
//!
//! `PartialEq` here is bit equality (same encoding), not any JavaScript
//! equality operator. JS `==`, `===` and SameValue live in the runtime.

use core::fmt;

const TAG_SHIFT: u32 = 48;
const PAYLOAD_MASK: u64 = (1 << TAG_SHIFT) - 1;
const LOW32: u64 = 0xFFFF_FFFF;

/// The single NaN every double NaN is folded into.
pub const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

/// First top-16-bit value reserved for tagged (non-double) values.
const FIRST_TAG: u64 = 0xFFF9;

const TAG_UNDEFINED: u64 = 0xFFF9;
const TAG_NULL: u64 = 0xFFFA;
const TAG_BOOL: u64 = 0xFFFB;
const TAG_INT32: u64 = 0xFFFC;
const TAG_OBJECT: u64 = 0xFFFD;
const TAG_STRING: u64 = 0xFFFE;
const TAG_OTHER: u64 = 0xFFFF;

/// A 32-bit offset into the engine instance's heap cage (D-002).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HeapRef(pub u32);

/// Kinds of heap values that are neither objects nor strings.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u16)]
pub enum OtherKind {
    Symbol = 1,
    BigInt = 2,
}

impl OtherKind {
    fn from_bits(bits: u16) -> Option<Self> {
        match bits {
            1 => Some(Self::Symbol),
            2 => Some(Self::BigInt),
            _ => None,
        }
    }
}

/// What a [`Value`] holds, for matching.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Undefined,
    Null,
    Bool(bool),
    Int32(i32),
    Double(f64),
    Object(HeapRef),
    String(HeapRef),
    Other(OtherKind, HeapRef),
}

/// A JavaScript value in 64 bits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Value(u64);

impl Value {
    pub const UNDEFINED: Value = Value(TAG_UNDEFINED << TAG_SHIFT);
    pub const NULL: Value = Value(TAG_NULL << TAG_SHIFT);
    pub const TRUE: Value = Value((TAG_BOOL << TAG_SHIFT) | 1);
    pub const FALSE: Value = Value(TAG_BOOL << TAG_SHIFT);

    #[inline]
    const fn tagged(tag: u64, payload: u64) -> Value {
        Value((tag << TAG_SHIFT) | (payload & PAYLOAD_MASK))
    }

    #[inline]
    const fn tag(self) -> u64 {
        self.0 >> TAG_SHIFT
    }

    /// Stores a double exactly as given. NaNs are canonicalized; -0 is kept.
    #[inline]
    pub fn from_f64(d: f64) -> Value {
        if d.is_nan() {
            Value(CANONICAL_NAN)
        } else {
            Value(d.to_bits())
        }
    }

    /// Stores a JS number, preferring the int32 form when it is exact.
    /// -0 stays a double, because int32 cannot represent it.
    #[inline]
    pub fn number(d: f64) -> Value {
        let i = d as i32;
        if i as f64 == d && !(d == 0.0 && d.is_sign_negative()) {
            Value::from_i32(i)
        } else {
            Value::from_f64(d)
        }
    }

    #[inline]
    pub const fn from_i32(i: i32) -> Value {
        Value::tagged(TAG_INT32, i as u32 as u64)
    }

    #[inline]
    pub const fn from_bool(b: bool) -> Value {
        if b {
            Value::TRUE
        } else {
            Value::FALSE
        }
    }

    #[inline]
    pub const fn object(r: HeapRef) -> Value {
        Value::tagged(TAG_OBJECT, r.0 as u64)
    }

    #[inline]
    pub const fn string(r: HeapRef) -> Value {
        Value::tagged(TAG_STRING, r.0 as u64)
    }

    #[inline]
    pub const fn other(kind: OtherKind, r: HeapRef) -> Value {
        Value::tagged(TAG_OTHER, ((kind as u64) << 32) | r.0 as u64)
    }

    /// The raw 64-bit encoding (for the JIT, cache files and debugging).
    #[inline]
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// Rebuilds a value from raw bits, rejecting encodings this module never
    /// produces. Cache files and other untrusted input must come through here.
    pub fn from_bits_checked(bits: u64) -> Option<Value> {
        let v = Value(bits);
        let tag = v.tag();
        let payload = bits & PAYLOAD_MASK;
        let ok = match tag {
            0xFFF8 => false, // NaN space not used by canonical doubles or tags
            t if t < FIRST_TAG => {
                let d = f64::from_bits(bits);
                !d.is_nan() || bits == CANONICAL_NAN
            }
            TAG_UNDEFINED | TAG_NULL => payload == 0,
            TAG_BOOL => payload <= 1,
            TAG_INT32 | TAG_OBJECT | TAG_STRING => payload <= LOW32,
            TAG_OTHER => OtherKind::from_bits((payload >> 32) as u16).is_some(),
            _ => false,
        };
        ok.then_some(v)
    }

    #[inline]
    pub const fn is_double(self) -> bool {
        self.tag() < FIRST_TAG
    }

    #[inline]
    pub const fn is_int32(self) -> bool {
        self.tag() == TAG_INT32
    }

    #[inline]
    pub const fn is_number(self) -> bool {
        self.is_double() || self.is_int32()
    }

    #[inline]
    pub const fn is_undefined(self) -> bool {
        self.0 == Value::UNDEFINED.0
    }

    #[inline]
    pub const fn is_null(self) -> bool {
        self.0 == Value::NULL.0
    }

    /// `null` or `undefined`.
    #[inline]
    pub const fn is_nullish(self) -> bool {
        self.is_undefined() || self.is_null()
    }

    /// The payload of an int32 value; `None` for every other kind
    /// (including doubles that happen to hold an integer).
    #[inline]
    pub const fn as_int32(self) -> Option<i32> {
        if self.is_int32() {
            Some((self.0 & LOW32) as u32 as i32)
        } else {
            None
        }
    }

    /// The payload of a boolean value; `None` for every other kind.
    #[inline]
    pub const fn as_bool(self) -> Option<bool> {
        if self.tag() == TAG_BOOL {
            Some(self.0 & 1 == 1)
        } else {
            None
        }
    }

    /// The numeric value of an int32 or double, as f64.
    #[inline]
    pub fn as_number(self) -> Option<f64> {
        match self.kind() {
            Kind::Int32(i) => Some(i as f64),
            Kind::Double(d) => Some(d),
            _ => None,
        }
    }

    /// Decodes the value for matching.
    pub fn kind(self) -> Kind {
        let payload = self.0 & PAYLOAD_MASK;
        match self.tag() {
            t if t < FIRST_TAG => Kind::Double(f64::from_bits(self.0)),
            TAG_UNDEFINED => Kind::Undefined,
            TAG_NULL => Kind::Null,
            TAG_BOOL => Kind::Bool(payload & 1 == 1),
            TAG_INT32 => Kind::Int32(payload as u32 as i32),
            TAG_OBJECT => Kind::Object(HeapRef(payload as u32)),
            TAG_STRING => Kind::String(HeapRef(payload as u32)),
            _ => {
                // Constructors only ever store valid kinds.
                let kind = OtherKind::from_bits((payload >> 32) as u16)
                    .expect("invariant: TAG_OTHER always carries a valid kind");
                Kind::Other(kind, HeapRef(payload as u32))
            }
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Value({:?} = {:#018x})", self.kind(), self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int32_and_bool_accessors() {
        for i in [0, 1, -1, i32::MIN, i32::MAX] {
            assert_eq!(Value::from_i32(i).as_int32(), Some(i));
        }
        assert_eq!(Value::from_f64(1.0).as_int32(), None);
        assert_eq!(Value::TRUE.as_int32(), None);
        assert_eq!(Value::TRUE.as_bool(), Some(true));
        assert_eq!(Value::FALSE.as_bool(), Some(false));
        assert_eq!(Value::from_i32(1).as_bool(), None);
        assert_eq!(Value::UNDEFINED.as_bool(), None);
    }

    #[test]
    fn singletons_round_trip() {
        assert_eq!(Value::UNDEFINED.kind(), Kind::Undefined);
        assert_eq!(Value::NULL.kind(), Kind::Null);
        assert_eq!(Value::TRUE.kind(), Kind::Bool(true));
        assert_eq!(Value::FALSE.kind(), Kind::Bool(false));
        assert!(Value::UNDEFINED.is_nullish() && Value::NULL.is_nullish());
        assert!(!Value::FALSE.is_nullish());
    }

    #[test]
    fn int32_bounds_round_trip() {
        for i in [0, 1, -1, i32::MIN, i32::MAX, 42, -42] {
            assert_eq!(Value::from_i32(i).kind(), Kind::Int32(i));
            assert_eq!(Value::from_i32(i).as_number(), Some(i as f64));
        }
    }

    #[test]
    fn special_doubles() {
        for d in [
            0.0,
            1.5,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
            f64::MAX,
            -f64::MAX,
            5e-324,
        ] {
            assert_eq!(Value::from_f64(d).kind(), Kind::Double(d));
        }
        let neg_zero = Value::from_f64(-0.0);
        match neg_zero.kind() {
            Kind::Double(d) => assert!(d == 0.0 && d.is_sign_negative()),
            k => panic!("expected double, got {k:?}"),
        }
    }

    #[test]
    fn all_nans_canonicalize() {
        let nans = [
            f64::NAN,
            -f64::NAN,
            f64::from_bits(0x7FF0_0000_0000_0001),
            f64::from_bits(0xFFFF_FFFF_FFFF_FFFF),
        ];
        for n in nans {
            let v = Value::from_f64(n);
            assert_eq!(v.to_bits(), CANONICAL_NAN);
            assert!(v.is_double());
        }
    }

    #[test]
    fn number_prefers_int32_but_keeps_negative_zero() {
        assert!(Value::number(7.0).is_int32());
        assert!(Value::number(-2147483648.0).is_int32());
        assert!(Value::number(2147483648.0).is_double());
        assert!(Value::number(0.5).is_double());
        assert!(Value::number(-0.0).is_double());
        assert!(Value::number(f64::NAN).is_double());
    }

    #[test]
    fn heap_refs_round_trip() {
        for off in [0u32, 1, 0x1234_5678, u32::MAX] {
            let r = HeapRef(off);
            assert_eq!(Value::object(r).kind(), Kind::Object(r));
            assert_eq!(Value::string(r).kind(), Kind::String(r));
            assert_eq!(
                Value::other(OtherKind::Symbol, r).kind(),
                Kind::Other(OtherKind::Symbol, r)
            );
            assert_eq!(
                Value::other(OtherKind::BigInt, r).kind(),
                Kind::Other(OtherKind::BigInt, r)
            );
        }
    }

    #[test]
    fn kinds_never_collide() {
        let r = HeapRef(0);
        let vals = [
            Value::UNDEFINED,
            Value::NULL,
            Value::FALSE,
            Value::from_i32(0),
            Value::from_f64(0.0),
            Value::object(r),
            Value::string(r),
            Value::other(OtherKind::Symbol, r),
            Value::other(OtherKind::BigInt, r),
        ];
        for (i, a) in vals.iter().enumerate() {
            for b in &vals[i + 1..] {
                assert_ne!(a.to_bits(), b.to_bits(), "{a:?} vs {b:?}");
            }
        }
    }

    /// Fuzz-style check: a million pseudo-random bit patterns as doubles must
    /// always come back as the same double (or canonical NaN), never a tag.
    #[test]
    fn random_doubles_stay_doubles() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..1_000_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let d = f64::from_bits(x);
            let v = Value::from_f64(d);
            assert!(v.is_double());
            match v.kind() {
                Kind::Double(back) if d.is_nan() => assert!(back.is_nan()),
                Kind::Double(back) => assert_eq!(back.to_bits(), d.to_bits()),
                k => panic!("double {x:#x} decoded as {k:?}"),
            }
            assert_eq!(Value::from_bits_checked(v.to_bits()), Some(v));
        }
    }

    #[test]
    fn checked_decode_rejects_bad_bits() {
        assert_eq!(Value::from_bits_checked(0xFFF8_0000_0000_0000), None); // reserved
        assert_eq!(Value::from_bits_checked(0x7FF0_0000_0000_0001), None); // non-canonical NaN
        assert_eq!(Value::from_bits_checked((TAG_BOOL << TAG_SHIFT) | 2), None);
        assert_eq!(Value::from_bits_checked((TAG_NULL << TAG_SHIFT) | 1), None);
        assert_eq!(
            Value::from_bits_checked((TAG_INT32 << TAG_SHIFT) | (1 << 40)),
            None
        );
        assert_eq!(
            Value::from_bits_checked((TAG_OTHER << TAG_SHIFT) | (9 << 32)),
            None
        );
        assert_eq!(
            Value::from_bits_checked(Value::TRUE.to_bits()),
            Some(Value::TRUE)
        );
    }
}
