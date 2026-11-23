//! Ordinary objects with named data properties (ARCHITECTURE.md §7.1,
//! D-038).
//!
//! Layout:
//!
//! ```text
//!  0  header    kind | inline slot count << 16 | shape << 32
//!  8  elements  indexed-property store (0 until task 1.8c)
//! 12  slots     out-of-line slots cell (0 until needed)
//! 16  inline    N value slots
//! ```
//!
//! A property's slot index comes from the shape; slots below N are
//! inline, the rest live in the out-of-line `Slots` cell (header: kind |
//! capacity << 32, then the values), which grows by doubling. Property
//! keys are atoms. Getters, setters, deletion, attribute changes and
//! dictionary mode come with task 1.8b; indexed properties with 1.8c.

use ferrojs_core::gc::{Root, Scope};
use ferrojs_core::{AllocError, Gc, NoGc, Unbound};
use ferrojs_value::Value;

use crate::shape::{self, Attrs, Property};
use crate::{CellKind, JsString, RootedValue, Shape, Tables};

/// Type marker for object cells.
pub struct Object;

/// Type marker for out-of-line slot cells.
pub struct Slots;

pub(crate) const ELEMENTS: u32 = 8;
pub(crate) const SLOTS: u32 = 12;
pub(crate) const INLINE: u32 = 16;

/// Inline slots of a plain object (ARCHITECTURE.md §7.1).
pub const DEFAULT_INLINE: u32 = 4;
/// At most this many inline slots (a limit on object size; the header
/// field itself has 16 bits).
pub const MAX_INLINE: u32 = 256;
/// Out-of-line slots start with this capacity.
const FIRST_SLOTS: u32 = 4;

pub(crate) fn inline_count(header: u64) -> u32 {
    ((header >> 16) & 0xffff) as u32
}

fn header(nogc: NoGc<'_>, o: Gc<'_, Object>) -> u64 {
    let h = nogc.read_u64(o, 0);
    debug_assert_eq!(h as u8, CellKind::Object as u8, "not an object cell");
    h
}

/// Allocates an empty ordinary object with prototype `proto` and
/// `inline` inline slots (at most [`MAX_INLINE`]).
pub fn new_object(
    s: &mut Scope<'_>,
    t: &mut Tables,
    proto: Option<Root<'_, Object>>,
    inline: u32,
) -> Result<Unbound<Object>, AllocError> {
    let inline = inline.min(MAX_INLINE);
    let root = shape::root_shape(s, t, proto, inline)?.bind(s.nogc());
    let root = s.root(root);
    let cell = s.alloc::<Object>(INLINE + 8 * inline)?;
    let nogc = s.nogc();
    let o = cell.bind(nogc);
    let undefined = Value::UNDEFINED.to_bits();
    for i in 0..inline {
        nogc.write_u64(o, INLINE + 8 * i, undefined);
    }
    nogc.write_u64(o, 0, CellKind::Object as u64 | (inline as u64) << 16);
    nogc.write_ref(o, 4, Some(root.get(nogc)));
    Ok(o.unbind())
}

/// The object's shape.
pub fn shape<'a>(nogc: NoGc<'a>, o: Gc<'_, Object>) -> Gc<'a, Shape> {
    let _ = header(nogc, o);
    nogc.read_ref(o, 4).expect("objects always have a shape")
}

/// The object's prototype (\[\[GetPrototypeOf\]\] of an ordinary object).
pub fn prototype<'a>(nogc: NoGc<'a>, o: Gc<'_, Object>) -> Option<Gc<'a, Object>> {
    shape::prototype(nogc, shape(nogc, o))
}

fn read_slot(nogc: NoGc<'_>, o: Gc<'_, Object>, slot: u32) -> Value {
    let inline = inline_count(header(nogc, o));
    let bits = if slot < inline {
        nogc.read_u64(o, INLINE + 8 * slot)
    } else {
        let slots: Gc<'_, Slots> = nogc.read_ref(o, SLOTS).expect("slot past the inline ones");
        nogc.read_u64(slots, 8 + 8 * (slot - inline))
    };
    Value::from_bits_checked(bits).expect("object slots hold valid values")
}

fn write_slot(
    nogc: NoGc<'_>,
    o: Gc<'_, Object>,
    slot: u32,
    bits: u64,
    referent: Option<Gc<'_, ()>>,
) {
    let inline = inline_count(header(nogc, o));
    if slot < inline {
        nogc.write_value_bits(o, INLINE + 8 * slot, bits, referent);
    } else {
        let slots: Gc<'_, Slots> = nogc.read_ref(o, SLOTS).expect("slot past the inline ones");
        nogc.write_value_bits(slots, 8 + 8 * (slot - inline), bits, referent);
    }
}

/// An own property: its value and attributes.
pub fn get_own(nogc: NoGc<'_>, o: Gc<'_, Object>, key: Gc<'_, JsString>) -> Option<(Value, Attrs)> {
    debug_assert!(crate::string::is_atom(nogc, key), "property keys are atoms");
    let p = shape::lookup(nogc, shape(nogc, o), key)?;
    Some((read_slot(nogc, o, p.slot), p.attrs))
}

/// \[\[Get\]\] for data properties: own, then up the prototype chain;
/// `undefined` if absent. `key` must be an atom.
pub fn get(nogc: NoGc<'_>, o: Gc<'_, Object>, key: Gc<'_, JsString>) -> Value {
    debug_assert!(crate::string::is_atom(nogc, key), "property keys are atoms");
    let mut cur = Some(o.cast::<Object>());
    while let Some(obj) = cur {
        if let Some((v, _)) = get_own(nogc, obj, key) {
            return v;
        }
        cur = prototype(nogc, obj);
    }
    Value::UNDEFINED
}

/// The own property keys in insertion order.
pub fn own_keys<'a>(nogc: NoGc<'a>, o: Gc<'_, Object>) -> Vec<Gc<'a, JsString>> {
    shape::keys(nogc, shape(nogc, o))
}

/// \[\[Set\]\] with the object as receiver, for data properties
/// (OrdinarySet): an own writable property is updated; a non-writable one
/// (own or inherited) refuses; otherwise the property is added with the
/// default attributes. Returns whether it succeeded (strict code throws a
/// `TypeError` on `false`). `key` must be an atom.
pub fn set(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    key: Root<'_, JsString>,
    value: RootedValue<'_>,
) -> Result<bool, AllocError> {
    let nogc = s.nogc();
    let (obj, k) = (o.get(nogc), key.get(nogc));
    debug_assert!(crate::string::is_atom(nogc, k), "property keys are atoms");
    if let Some(p) = shape::lookup(nogc, shape(nogc, obj), k) {
        if !p.attrs.contains(Attrs::WRITABLE) {
            return Ok(false);
        }
        let (bits, referent) = value.resolve(nogc);
        write_slot(nogc, obj, p.slot, bits, referent);
        return Ok(true);
    }
    // An inherited non-writable property blocks the assignment.
    let mut proto = prototype(nogc, obj);
    while let Some(p) = proto {
        if let Some((_, attrs)) = get_own(nogc, p, k) {
            if !attrs.contains(Attrs::WRITABLE) {
                return Ok(false);
            }
            break;
        }
        proto = prototype(nogc, p);
    }
    add(s, o, key, value, Attrs::DEFAULT)?;
    Ok(true)
}

/// Defines a new own data property with `attrs`, or updates the value of
/// an existing one whose attributes are the same. Changing the attributes
/// of an existing property needs dictionary mode (task 1.8b) and returns
/// `Ok(false)` for now. `key` must be an atom.
pub fn define(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    key: Root<'_, JsString>,
    value: RootedValue<'_>,
    attrs: Attrs,
) -> Result<bool, AllocError> {
    let nogc = s.nogc();
    let (obj, k) = (o.get(nogc), key.get(nogc));
    debug_assert!(crate::string::is_atom(nogc, k), "property keys are atoms");
    match shape::lookup(nogc, shape(nogc, obj), k) {
        Some(Property { slot, attrs: a }) if a == attrs => {
            let (bits, referent) = value.resolve(nogc);
            // ValidateAndApplyPropertyDescriptor: a non-configurable,
            // non-writable property only accepts its own value (SameValue).
            if !a.contains(Attrs::WRITABLE) && !a.contains(Attrs::CONFIGURABLE) {
                let old = read_slot(nogc, obj, slot);
                let new = Value::from_bits_checked(bits).expect("valid value");
                return Ok(same_value(nogc, old, new));
            }
            write_slot(nogc, obj, slot, bits, referent);
            Ok(true)
        }
        Some(_) => Ok(false),
        None => {
            add(s, o, key, value, attrs)?;
            Ok(true)
        }
    }
}

/// Adds a property that isn't there yet: transition, make room, store.
fn add(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    key: Root<'_, JsString>,
    value: RootedValue<'_>,
    attrs: Attrs,
) -> Result<(), AllocError> {
    let old = s.root(shape(s.nogc(), o.get(s.nogc())));
    let new_shape = shape::add_property(s, old, key, attrs)?.bind(s.nogc());
    let new_shape = s.root(new_shape);
    let slot = shape::count(s.nogc(), new_shape.get(s.nogc())) - 1;
    ensure_slot(s, o, slot)?;
    let nogc = s.nogc();
    let obj = o.get(nogc);
    let (bits, referent) = value.resolve(nogc);
    write_slot(nogc, obj, slot, bits, referent);
    nogc.write_ref(obj, 4, Some(new_shape.get(nogc)));
    Ok(())
}

/// Makes sure `slot` has storage, growing the out-of-line slots.
fn ensure_slot(s: &mut Scope<'_>, o: Root<'_, Object>, slot: u32) -> Result<(), AllocError> {
    let nogc = s.nogc();
    let obj = o.get(nogc);
    let inline = inline_count(header(nogc, obj));
    if slot < inline {
        return Ok(());
    }
    let index = slot - inline;
    let old: Option<Gc<'_, Slots>> = nogc.read_ref(obj, SLOTS);
    let cap = old.map_or(0, |c| (nogc.read_u64(c, 0) >> 32) as u32);
    if index < cap {
        return Ok(());
    }
    let new_cap = cap.saturating_mul(2).max(FIRST_SLOTS).max(index + 1);
    let size = 8u32
        .checked_add(
            new_cap
                .checked_mul(8)
                .ok_or(AllocError::BadSize(u64::MAX))?,
        )
        .ok_or(AllocError::BadSize(u64::MAX))?;
    let cell = s.alloc::<Slots>(size)?;
    let nogc = s.nogc();
    let fresh = cell.bind(nogc);
    let obj = o.get(nogc);
    let old: Option<Gc<'_, Slots>> = nogc.read_ref(obj, SLOTS);
    let undefined = Value::UNDEFINED.to_bits();
    for i in 0..new_cap {
        let bits = match old {
            Some(c) if i < cap => nogc.read_u64(c, 8 + 8 * i),
            _ => undefined,
        };
        // An initializing store into a cell allocated just now: a
        // generational barrier (Phase 5) treats new cells as young, so
        // these stores need no referent.
        nogc.write_value_bits(fresh, 8 + 8 * i, bits, None::<Gc<'_, ()>>);
    }
    nogc.write_u64(fresh, 0, CellKind::Slots as u64 | (new_cap as u64) << 32);
    nogc.write_ref(obj, SLOTS, Some(fresh));
    Ok(())
}

/// SameValue (ECMA-262 §7.2.10) for the values the runtime has so far:
/// numbers by value (NaN equals NaN, +0 differs from -0), strings by
/// contents, everything else by identity.
pub fn same_value(nogc: NoGc<'_>, a: Value, b: Value) -> bool {
    use ferrojs_value::Kind;
    if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
        return (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits();
    }
    match (a.kind(), b.kind()) {
        (Kind::String(x), Kind::String(y)) => {
            // Different cells may hold the same text. Both values come from
            // live slots or roots, so both offsets are live string cells.
            let cage = nogc.cage();
            x == y || crate::string::units_at(cage, x.0) == crate::string::units_at(cage, y.0)
        }
        _ => a == b,
    }
}
