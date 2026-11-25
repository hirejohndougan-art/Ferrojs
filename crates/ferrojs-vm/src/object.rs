//! Ordinary objects with named data and accessor properties
//! (ARCHITECTURE.md §7.1, D-038, D-055).
//!
//! Layout:
//!
//! ```text
//!  0  header    kind | flags << 8 | inline slot count << 16 | shape << 32
//!  8  elements  indexed-property store (0 until task 1.8c)
//! 12  slots     out-of-line slots cell (0 until needed)
//! 16  inline    N value slots
//! 16 + 8N       functions only: internal word (see below)
//! ```
//!
//! Header flags: bit 8 marks an array (`elements::ARRAY`), bit 9 a
//! closure ([`CLOSURE`]), bit 10 a host function ([`HOST`]). Functions are
//! ordinary objects with one extra 8-byte internal word after the inline
//! slots (task 1.10a, D-057): a closure's holds its template index (low
//! half) and its environment cell (high half, 0 for none, traced); a host
//! function's holds its index in the host function table.
//!
//! A property's slot index comes from the shape; slots below N are
//! inline, the rest live in the out-of-line `Slots` cell (header: kind |
//! capacity << 32, then the values), which grows by doubling. Property
//! keys are atoms. Deletion, attribute changes, non-extensible objects
//! and dictionary mode came with task 1.8b (D-039), indexed properties
//! with 1.8c.
//!
//! An accessor property (task 1.8d, D-055) has [`Attrs::ACCESSOR`] and
//! its slot holds an object-tagged reference to an accessor pair cell
//! (kind 11, 24 bytes: header, getter value, setter value; each a
//! function or `undefined`). Every pair belongs to one property of one
//! object, so changing one half updates it in place. This module never
//! calls code: [`get`] and [`set`] say which getter or setter the caller
//! has to call ([`Got::Getter`], [`SetResult::Setter`]).

use ferrojs_core::gc::{Root, Scope};
use ferrojs_core::{AllocError, Gc, NoGc, Unbound};
use ferrojs_value::Value;

use crate::shape::{self, Attrs, Property};
use crate::{CellKind, JsString, RootedValue, Shape, Tables};

/// Type marker for object cells.
pub struct Object;

/// Type marker for out-of-line slot cells.
pub struct Slots;

/// Type marker for accessor pair cells.
pub struct AccessorPair;

pub(crate) const ACCESSOR_GET: u32 = 8;
pub(crate) const ACCESSOR_SET: u32 = 16;
pub(crate) const ACCESSOR_SIZE: u32 = 24;

/// An own property's contents.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PropertyValue {
    Data(Value),
    /// Getter and setter, each a function or `undefined`.
    Accessor {
        get: Value,
        set: Value,
    },
}

/// What \[\[Get\]\] found.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Got {
    /// The value (`undefined` when absent or when the accessor has no
    /// getter).
    Value(Value),
    /// An accessor's getter: the result is calling it with the receiver
    /// as `this` and no arguments.
    Getter(Value),
}

/// What \[\[Set\]\] did.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SetResult {
    /// Stored (`true`) or refused (`false`; strict code throws a
    /// `TypeError`).
    Done(bool),
    /// An accessor's setter: the assignment is calling it with the
    /// receiver as `this` and the value as its argument (it succeeds
    /// whatever the setter returns).
    Setter(Value),
}

impl SetResult {
    /// The outcome of a set that can't meet a setter (on an object the
    /// caller knows has no accessors).
    #[track_caller]
    pub fn unwrap_done(self) -> bool {
        match self {
            SetResult::Done(ok) => ok,
            SetResult::Setter(_) => panic!("set reached a setter"),
        }
    }
}

/// A property descriptor (ECMA-262 §6.2.6) with each field present or
/// not. It can't be both a data and an accessor descriptor; `get` and
/// `set` must be functions or `undefined` (ToPropertyDescriptor checks
/// both before calling [`define_property`]).
#[derive(Clone, Copy, Default)]
pub struct Descriptor<'s> {
    pub value: Option<RootedValue<'s>>,
    pub writable: Option<bool>,
    pub get: Option<RootedValue<'s>>,
    pub set: Option<RootedValue<'s>>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl<'s> Descriptor<'s> {
    /// A complete data descriptor.
    pub fn data(value: RootedValue<'s>, attrs: Attrs) -> Descriptor<'s> {
        debug_assert!(!attrs.contains(Attrs::ACCESSOR));
        Descriptor {
            value: Some(value),
            writable: Some(attrs.contains(Attrs::WRITABLE)),
            enumerable: Some(attrs.contains(Attrs::ENUMERABLE)),
            configurable: Some(attrs.contains(Attrs::CONFIGURABLE)),
            ..Descriptor::default()
        }
    }

    /// A complete accessor descriptor (`enumerable` and `configurable`
    /// from `attrs`).
    pub fn accessor(get: RootedValue<'s>, set: RootedValue<'s>, attrs: Attrs) -> Descriptor<'s> {
        Descriptor {
            get: Some(get),
            set: Some(set),
            enumerable: Some(attrs.contains(Attrs::ENUMERABLE)),
            configurable: Some(attrs.contains(Attrs::CONFIGURABLE)),
            ..Descriptor::default()
        }
    }

    pub fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    pub fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    fn is_empty(&self) -> bool {
        !self.is_accessor()
            && !self.is_data()
            && self.enumerable.is_none()
            && self.configurable.is_none()
    }
}

pub(crate) const ELEMENTS: u32 = 8;
pub(crate) const SLOTS: u32 = 12;
pub(crate) const INLINE: u32 = 16;

/// Inline slots of a plain object (ARCHITECTURE.md §7.1).
pub const DEFAULT_INLINE: u32 = 4;
/// At most this many inline slots (a limit on object size; the header
/// field itself has 16 bits).
pub const MAX_INLINE: u32 = 256;
/// Header flag of a closure (an object with a template and an
/// environment).
pub const CLOSURE: u64 = 1 << 9;
/// Header flag of a host function.
pub const HOST: u64 = 1 << 10;

/// Out-of-line slots start with this capacity.
const FIRST_SLOTS: u32 = 4;

#[inline]
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

/// Allocates a function object of class `class` ([`CLOSURE`] or
/// [`HOST`]) with prototype `proto` and `inline` inline slots, its
/// internal word zero. The caller fills the internal word
/// ([`internal_word`]) before the next allocation.
pub fn new_function_object(
    s: &mut Scope<'_>,
    t: &mut Tables,
    proto: Root<'_, Object>,
    inline: u32,
    class: u64,
) -> Result<Unbound<Object>, AllocError> {
    debug_assert!(class == CLOSURE || class == HOST);
    let inline = inline.min(MAX_INLINE);
    let root = shape::root_shape(s, t, Some(proto), inline)?.bind(s.nogc());
    let root = s.root(root);
    let cell = s.alloc::<Object>(INLINE + 8 * inline + 8)?;
    let nogc = s.nogc();
    let o = cell.bind(nogc);
    let undefined = Value::UNDEFINED.to_bits();
    for i in 0..inline {
        nogc.write_u64(o, INLINE + 8 * i, undefined);
    }
    nogc.write_u64(o, INLINE + 8 * inline, 0);
    nogc.write_u64(
        o,
        0,
        CellKind::Object as u64 | class | (inline as u64) << 16,
    );
    nogc.write_ref(o, 4, Some(root.get(nogc)));
    Ok(o.unbind())
}

/// Offset of function object `o`'s internal word.
pub fn internal_word(nogc: NoGc<'_>, o: Gc<'_, Object>) -> u32 {
    let h = header(nogc, o);
    debug_assert!(h & (CLOSURE | HOST) != 0, "not a function object");
    INLINE + 8 * inline_count(h)
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

/// The accessor pair an accessor property's slot refers to.
fn pair_of<'a>(nogc: NoGc<'a>, slot_value: Value) -> Gc<'a, AccessorPair> {
    let r = crate::value_ref(slot_value.to_bits()).expect("accessor slots hold a pair");
    let pair: Gc<'_, AccessorPair> = nogc.handle(r).expect("accessor pairs are live");
    debug_assert_eq!(
        nogc.read_u64(pair, 0) as u8,
        CellKind::Accessor as u8,
        "not an accessor pair"
    );
    pair
}

fn pair_half(nogc: NoGc<'_>, pair: Gc<'_, AccessorPair>, field: u32) -> Value {
    Value::from_bits_checked(nogc.read_u64(pair, field)).expect("pairs hold valid values")
}

fn read_property(nogc: NoGc<'_>, o: Gc<'_, Object>, p: Property) -> PropertyValue {
    let v = read_slot(nogc, o, p.slot);
    if !p.attrs.contains(Attrs::ACCESSOR) {
        return PropertyValue::Data(v);
    }
    let pair = pair_of(nogc, v);
    PropertyValue::Accessor {
        get: pair_half(nogc, pair, ACCESSOR_GET),
        set: pair_half(nogc, pair, ACCESSOR_SET),
    }
}

/// An own property: its contents and attributes.
pub fn get_own(
    nogc: NoGc<'_>,
    o: Gc<'_, Object>,
    key: Gc<'_, JsString>,
) -> Option<(PropertyValue, Attrs)> {
    debug_assert!(crate::string::is_atom(nogc, key), "property keys are atoms");
    let p = shape::lookup(nogc, shape(nogc, o), key)?;
    Some((read_property(nogc, o, p), p.attrs))
}

/// \[\[Get\]\] with the object as receiver (OrdinaryGet): own, then up
/// the prototype chain; `undefined` if absent. An accessor's getter is
/// for the caller to call. `key` must be an atom.
pub fn get(nogc: NoGc<'_>, o: Gc<'_, Object>, key: Gc<'_, JsString>) -> Got {
    debug_assert!(crate::string::is_atom(nogc, key), "property keys are atoms");
    let mut cur = Some(o.cast::<Object>());
    while let Some(obj) = cur {
        match get_own(nogc, obj, key) {
            Some((PropertyValue::Data(v), _)) => return Got::Value(v),
            Some((PropertyValue::Accessor { get, .. }, _)) => {
                return if get.is_undefined() {
                    Got::Value(Value::UNDEFINED)
                } else {
                    Got::Getter(get)
                };
            }
            None => cur = prototype(nogc, obj),
        }
    }
    Got::Value(Value::UNDEFINED)
}

/// The own property keys in insertion order.
pub fn own_keys<'a>(nogc: NoGc<'a>, o: Gc<'_, Object>) -> Vec<Gc<'a, JsString>> {
    shape::keys(nogc, shape(nogc, o))
}

/// The outcome of assigning to an accessor property: call its setter,
/// or refuse if it has none.
fn setter_of(set: Value) -> SetResult {
    if set.is_undefined() {
        SetResult::Done(false)
    } else {
        SetResult::Setter(set)
    }
}

/// \[\[Set\]\] with the object as receiver (OrdinarySet): an own
/// writable data property is updated; a non-writable data property (own
/// or inherited) refuses; an accessor (own or inherited) hands back its
/// setter, or refuses without one; otherwise the property is added with
/// the default attributes. `key` must be an atom.
pub fn set(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    key: Root<'_, JsString>,
    value: RootedValue<'_>,
) -> Result<SetResult, AllocError> {
    let nogc = s.nogc();
    let (obj, k) = (o.get(nogc), key.get(nogc));
    debug_assert!(crate::string::is_atom(nogc, k), "property keys are atoms");
    if let Some(p) = shape::lookup(nogc, shape(nogc, obj), k) {
        if p.attrs.contains(Attrs::ACCESSOR) {
            let pair = pair_of(nogc, read_slot(nogc, obj, p.slot));
            return Ok(setter_of(pair_half(nogc, pair, ACCESSOR_SET)));
        }
        if !p.attrs.contains(Attrs::WRITABLE) {
            return Ok(SetResult::Done(false));
        }
        let (bits, referent) = value.resolve(nogc);
        write_slot(nogc, obj, p.slot, bits, referent);
        return Ok(SetResult::Done(true));
    }
    // An inherited accessor or non-writable property decides the
    // assignment; an inherited writable one doesn't.
    let mut proto = prototype(nogc, obj);
    while let Some(p) = proto {
        match get_own(nogc, p, k) {
            Some((PropertyValue::Accessor { set, .. }, _)) => return Ok(setter_of(set)),
            Some((PropertyValue::Data(_), attrs)) => {
                if !attrs.contains(Attrs::WRITABLE) {
                    return Ok(SetResult::Done(false));
                }
                break;
            }
            None => proto = prototype(nogc, p),
        }
    }
    if !is_extensible(s.nogc(), o.get(s.nogc())) {
        return Ok(SetResult::Done(false));
    }
    add(s, o, key, value, Attrs::DEFAULT)?;
    Ok(SetResult::Done(true))
}

/// Defines an own data property with `attrs` (no
/// [`Attrs::ACCESSOR`]) and `value`: [`define_property`] with a complete
/// data descriptor. Returns whether it succeeded. `key` must be an atom.
pub fn define(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    key: Root<'_, JsString>,
    value: RootedValue<'_>,
    attrs: Attrs,
) -> Result<bool, AllocError> {
    define_property(s, o, key, Descriptor::data(value, attrs))
}

/// A new accessor pair holding `get` and `set` (`undefined` if absent).
fn new_pair<'s, 'r>(
    s: &mut Scope<'s>,
    get: Option<RootedValue<'r>>,
    set: Option<RootedValue<'r>>,
) -> Result<Root<'s, AccessorPair>, AllocError> {
    let cell = s.alloc::<AccessorPair>(ACCESSOR_SIZE)?;
    let nogc = s.nogc();
    let pair = cell.bind(nogc);
    nogc.write_u64(pair, 0, CellKind::Accessor as u64);
    for (field, half) in [(ACCESSOR_GET, get), (ACCESSOR_SET, set)] {
        let (bits, referent) = half
            .unwrap_or(RootedValue::Primitive(Value::UNDEFINED))
            .resolve(nogc);
        nogc.write_value_bits(pair, field, bits, referent);
    }
    Ok(s.root(pair))
}

/// The slot value for `pair`: an object-tagged reference (the tracer
/// follows it like any other). Only accessor paths ever read it, and this
/// root is only ever stored, never used as an object.
fn pair_slot<'s>(s: &mut Scope<'s>, pair: Root<'_, AccessorPair>) -> RootedValue<'s> {
    let nogc = s.nogc();
    RootedValue::Object(s.root(pair.get(nogc).cast::<Object>()))
}

/// The value `v` names, for SameValue.
fn value_of(nogc: NoGc<'_>, v: RootedValue<'_>) -> Value {
    Value::from_bits_checked(v.resolve(nogc).0).expect("valid value")
}

/// \[\[DefineOwnProperty\]\] of an ordinary object
/// (ValidateAndApplyPropertyDescriptor, ECMA-262 §10.1.6.3). A new
/// property needs an extensible object; absent fields default to
/// `undefined` and `false`. On an existing non-configurable property the
/// descriptor can't make it configurable, change its enumerability or
/// turn it between data and accessor; for an accessor it can't change
/// the getter or setter (SameValue), and for a non-writable data
/// property it can't make it writable or change its value. Otherwise
/// present fields replace the current ones (a change of kind keeps
/// enumerable and configurable and resets the rest); an attribute change
/// moves the object to dictionary mode. Returns whether it succeeded.
/// `key` must be an atom.
///
/// # Panics
///
/// If `desc` is both a data and an accessor descriptor.
pub fn define_property(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    key: Root<'_, JsString>,
    desc: Descriptor<'_>,
) -> Result<bool, AllocError> {
    assert!(
        !(desc.is_accessor() && desc.is_data()),
        "a descriptor is either data or accessor"
    );
    let nogc = s.nogc();
    let (obj, k) = (o.get(nogc), key.get(nogc));
    debug_assert!(crate::string::is_atom(nogc, k), "property keys are atoms");
    let sh = shape(nogc, obj);
    let flag = |b: Option<bool>, a: Attrs| if b == Some(true) { a } else { Attrs(0) };
    let Some(current) = shape::lookup(nogc, sh, k) else {
        if !shape::is_extensible(nogc, sh) {
            return Ok(false);
        }
        let attrs = flag(desc.enumerable, Attrs::ENUMERABLE)
            .with(flag(desc.configurable, Attrs::CONFIGURABLE));
        if desc.is_accessor() {
            let pair = new_pair(s, desc.get, desc.set)?;
            let slot = pair_slot(s, pair);
            add(s, o, key, slot, attrs.with(Attrs::ACCESSOR))?;
        } else {
            let value = desc
                .value
                .unwrap_or(RootedValue::Primitive(Value::UNDEFINED));
            add(
                s,
                o,
                key,
                value,
                attrs.with(flag(desc.writable, Attrs::WRITABLE)),
            )?;
        }
        return Ok(true);
    };
    if desc.is_empty() {
        return Ok(true);
    }
    let cur = current.attrs;
    let cur_accessor = cur.contains(Attrs::ACCESSOR);
    let old = read_property(nogc, obj, current);
    if !cur.contains(Attrs::CONFIGURABLE) {
        if desc.configurable == Some(true)
            || desc
                .enumerable
                .is_some_and(|e| e != cur.contains(Attrs::ENUMERABLE))
        {
            return Ok(false);
        }
        let generic = !desc.is_accessor() && !desc.is_data();
        if !generic && desc.is_accessor() != cur_accessor {
            return Ok(false);
        }
        let differs = |new: Option<RootedValue<'_>>, old: Value| {
            new.is_some_and(|v| !same_value(nogc, value_of(nogc, v), old))
        };
        match old {
            PropertyValue::Accessor { get, set } => {
                if differs(desc.get, get) || differs(desc.set, set) {
                    return Ok(false);
                }
            }
            PropertyValue::Data(v) => {
                if !cur.contains(Attrs::WRITABLE)
                    && (desc.writable == Some(true) || differs(desc.value, v))
                {
                    return Ok(false);
                }
            }
        }
    }
    let kept = |b: Option<bool>, a: Attrs| match b {
        Some(b) => flag(Some(b), a),
        None => Attrs(cur.0 & a.0),
    };
    let common =
        kept(desc.enumerable, Attrs::ENUMERABLE).with(kept(desc.configurable, Attrs::CONFIGURABLE));
    // What goes in the slot: a new pair (data to accessor), a value, or
    // the halves of the current pair to replace.
    enum Content<'p, 'r> {
        Pair(Root<'p, AccessorPair>),
        Value(RootedValue<'r>),
        Patch,
    }
    let (attrs, content) = if desc.is_accessor() && !cur_accessor {
        let pair = new_pair(s, desc.get, desc.set)?;
        (common.with(Attrs::ACCESSOR), Some(Content::Pair(pair)))
    } else if desc.is_data() && cur_accessor {
        let value = desc
            .value
            .unwrap_or(RootedValue::Primitive(Value::UNDEFINED));
        let writable = flag(desc.writable, Attrs::WRITABLE);
        (common.with(writable), Some(Content::Value(value)))
    } else if cur_accessor {
        let patch = desc.get.is_some() || desc.set.is_some();
        (
            common.with(Attrs::ACCESSOR),
            patch.then_some(Content::Patch),
        )
    } else {
        let writable = kept(desc.writable, Attrs::WRITABLE);
        (common.with(writable), desc.value.map(Content::Value))
    };
    if attrs != cur {
        let sh = to_own_dictionary(s, o)?;
        let nogc = s.nogc();
        shape::dict_set_attrs(nogc, sh.get(nogc), key.get(nogc), attrs);
    }
    let nogc = s.nogc();
    let obj = o.get(nogc);
    match content {
        None => {}
        Some(Content::Pair(pair)) => {
            let pair = pair.get(nogc);
            let bits = Value::object(ferrojs_value::HeapRef(pair.offset())).to_bits();
            write_slot(nogc, obj, current.slot, bits, Some(pair.cast()));
        }
        Some(Content::Value(v)) => {
            let (bits, referent) = v.resolve(nogc);
            write_slot(nogc, obj, current.slot, bits, referent);
        }
        Some(Content::Patch) => {
            let pair = pair_of(nogc, read_slot(nogc, obj, current.slot));
            for (field, half) in [(ACCESSOR_GET, desc.get), (ACCESSOR_SET, desc.set)] {
                if let Some(v) = half {
                    let (bits, referent) = v.resolve(nogc);
                    nogc.write_value_bits(pair, field, bits, referent);
                }
            }
        }
    }
    Ok(true)
}

/// \[\[Delete\]\] for own properties: removes `key` unless it is
/// non-configurable. Deleting the property added last steps back to the
/// parent shape; any other moves the object to dictionary mode. Returns
/// whether the property is gone.
pub fn delete(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    key: Root<'_, JsString>,
) -> Result<bool, AllocError> {
    let nogc = s.nogc();
    let (obj, k) = (o.get(nogc), key.get(nogc));
    debug_assert!(crate::string::is_atom(nogc, k), "property keys are atoms");
    let sh = shape(nogc, obj);
    let Some(p) = shape::lookup(nogc, sh, k) else {
        return Ok(true);
    };
    if !p.attrs.contains(Attrs::CONFIGURABLE) {
        return Ok(false);
    }
    let last = !shape::is_dictionary(nogc, sh)
        && shape::last_key(nogc, sh).map(|l| l.offset()) == Some(k.offset());
    if last {
        let parent = shape::parent(nogc, sh).expect("a shape with a key has a parent");
        nogc.write_ref(obj, 4, Some(parent));
    } else {
        let sh = to_own_dictionary(s, o)?;
        let nogc = s.nogc();
        shape::dict_remove(nogc, sh.get(nogc), key.get(nogc));
    }
    // Clear the slot so it doesn't keep its old value alive.
    let nogc = s.nogc();
    write_slot(
        nogc,
        o.get(nogc),
        p.slot,
        Value::UNDEFINED.to_bits(),
        None::<Gc<'_, ()>>,
    );
    Ok(true)
}

/// \[\[PreventExtensions\]\]: no new properties from now on. Moves the
/// object to dictionary mode (Phase 1 keeps the flag in its own shape).
pub fn prevent_extensions(s: &mut Scope<'_>, o: Root<'_, Object>) -> Result<(), AllocError> {
    let sh = to_own_dictionary(s, o)?;
    let nogc = s.nogc();
    shape::dict_prevent_extensions(nogc, sh.get(nogc));
    Ok(())
}

/// \[\[IsExtensible\]\].
pub fn is_extensible(nogc: NoGc<'_>, o: Gc<'_, Object>) -> bool {
    shape::is_extensible(nogc, shape(nogc, o))
}

/// Whether the object is in dictionary mode.
pub fn is_dictionary(nogc: NoGc<'_>, o: Gc<'_, Object>) -> bool {
    shape::is_dictionary(nogc, shape(nogc, o))
}

/// Puts the object in dictionary mode if it isn't; returns its (own)
/// dictionary shape, rooted.
fn to_own_dictionary<'s>(
    s: &mut Scope<'s>,
    o: Root<'_, Object>,
) -> Result<Root<'s, Shape>, AllocError> {
    let nogc = s.nogc();
    let sh = shape(nogc, o.get(nogc));
    if shape::is_dictionary(nogc, sh) {
        return Ok(s.root(sh));
    }
    let sh = s.root(sh);
    let dict = shape::to_dictionary(s, sh)?.bind(s.nogc());
    let nogc = s.nogc();
    nogc.write_ref(o.get(nogc), 4, Some(dict));
    Ok(s.root(dict))
}

/// Shared shapes with more properties than this switch to dictionary mode
/// (ARCHITECTURE.md §7.2).
pub const MAX_SHARED_PROPERTIES: u32 = 128;

/// Adds a property that isn't there yet: transition (or dictionary
/// entry), make room, store.
fn add(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    key: Root<'_, JsString>,
    value: RootedValue<'_>,
    attrs: Attrs,
) -> Result<(), AllocError> {
    let nogc = s.nogc();
    let sh = shape(nogc, o.get(nogc));
    if shape::is_dictionary(nogc, sh) || shape::count(nogc, sh) >= MAX_SHARED_PROPERTIES {
        let dict = to_own_dictionary(s, o)?;
        // Storage first: if it can't be had, the property isn't added.
        let slot = shape::dict_next_slot(s.nogc(), dict.get(s.nogc()));
        ensure_slot(s, o, slot)?;
        let got = shape::dict_add(s, dict, key, attrs)?;
        debug_assert_eq!(got, slot);
        let nogc = s.nogc();
        let (bits, referent) = value.resolve(nogc);
        write_slot(nogc, o.get(nogc), slot, bits, referent);
        return Ok(());
    }
    let old = s.root(sh);
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
