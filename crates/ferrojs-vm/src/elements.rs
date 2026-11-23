//! Indexed properties and arrays (ARCHITECTURE.md §7.1, D-040).
//!
//! An object's integer-indexed properties live in its elements store,
//! apart from its named properties: keys `0 ..= 2^32 - 2` (array
//! indices). The interpreter and built-ins route such keys here; named
//! keys never hold array indices. Elements are plain data properties
//! (writable, enumerable, configurable); other attributes on elements
//! come with frozen and sealed objects later.
//!
//! The store is an `Elements` cell (kind 6):
//!
//! ```text
//!  0  header   kind | flags << 8 | capacity << 32
//!  8  length   the array length (arrays only)
//! 12  count    sparse: entries in use
//! 16  data     dense: `capacity` values, holes hold the internal
//!              uninitialized marker; sparse: 16-byte entries of index
//!              (u32, then 4 unused bytes) and value, in no order
//! ```
//!
//! Dense stores grow by doubling. Writing at an index of twice the
//! capacity plus 8 or more (or of 2^20 or more) turns the store sparse for
//! good; sparse lookup is linear in Phase 1. An array is an ordinary
//! object with the `ARRAY` flag in its header; its `length` lives in the
//! elements header (callers expose it as the `length` property).

use ferrojs_core::gc::{Root, Scope};
use ferrojs_core::{AllocError, Gc, NoGc, Unbound};
use ferrojs_value::Value;

use crate::object::{self, Object};
use crate::{CellKind, RootedValue, Tables};

/// Type marker for elements cells.
pub struct Elements;

/// The largest array index.
pub const MAX_INDEX: u32 = u32::MAX - 1;
/// Dense stores never grow past this many slots.
const DENSE_LIMIT: u32 = 1 << 20;

pub(crate) const SPARSE: u64 = 1 << 8;
pub(crate) const LENGTH: u32 = 8;
pub(crate) const COUNT: u32 = 12;
pub(crate) const DATA: u32 = 16;
const ENTRY: u32 = 16;

/// Object header flag: the object is an Array exotic object.
pub(crate) const ARRAY: u64 = 1 << 8;

fn hole() -> u64 {
    Value::UNINITIALIZED.to_bits()
}

fn store<'a>(nogc: NoGc<'a>, o: Gc<'_, Object>) -> Option<Gc<'a, Elements>> {
    nogc.read_ref(o, object::ELEMENTS)
}

fn cap_of(nogc: NoGc<'_>, e: Gc<'_, Elements>) -> u32 {
    (nogc.read_u64(e, 0) >> 32) as u32
}

fn is_sparse(nogc: NoGc<'_>, e: Gc<'_, Elements>) -> bool {
    nogc.read_u64(e, 0) & SPARSE != 0
}

/// Whether `o` is an array.
pub fn is_array(nogc: NoGc<'_>, o: Gc<'_, Object>) -> bool {
    nogc.read_u64(o, 0) & ARRAY != 0
}

/// A new empty array with prototype `proto`.
pub fn new_array(
    s: &mut Scope<'_>,
    t: &mut Tables,
    proto: Option<Root<'_, Object>>,
) -> Result<Unbound<Object>, AllocError> {
    let o = object::new_object(s, t, proto, 0)?.bind(s.nogc());
    let o = s.root(o);
    let e = alloc_dense(s, 0)?;
    let nogc = s.nogc();
    let obj = o.get(nogc);
    let h = nogc.read_u64(obj, 0);
    nogc.write_u32(obj, 0, (h | ARRAY) as u32);
    nogc.write_ref(obj, object::ELEMENTS, Some(e.bind(nogc)));
    Ok(obj.unbind())
}

/// An array's length (0 for other objects).
pub fn length(nogc: NoGc<'_>, o: Gc<'_, Object>) -> u32 {
    match store(nogc, o) {
        Some(e) if is_array(nogc, o) => nogc.read_u32(e, LENGTH),
        _ => 0,
    }
}

fn alloc_dense(s: &mut Scope<'_>, cap: u32) -> Result<Unbound<Elements>, AllocError> {
    let size = DATA
        .checked_add(cap.checked_mul(8).ok_or(AllocError::BadSize(u64::MAX))?)
        .ok_or(AllocError::BadSize(u64::MAX))?;
    let cell = s.alloc::<Elements>(size)?;
    let nogc = s.nogc();
    let e = cell.bind(nogc);
    for i in 0..cap {
        nogc.write_u64(e, DATA + 8 * i, hole());
    }
    nogc.write_u64(e, 0, CellKind::Elements as u64 | (cap as u64) << 32);
    Ok(e.unbind())
}

/// The own element at `index`, if present.
pub fn get_own(nogc: NoGc<'_>, o: Gc<'_, Object>, index: u32) -> Option<Value> {
    let e = store(nogc, o)?;
    let bits = if is_sparse(nogc, e) {
        let i = sparse_find(nogc, e, index)?;
        nogc.read_u64(e, DATA + ENTRY * i + 8)
    } else {
        if index >= cap_of(nogc, e) {
            return None;
        }
        nogc.read_u64(e, DATA + 8 * index)
    };
    if bits == hole() {
        return None;
    }
    Some(Value::from_bits_checked(bits).expect("elements hold valid values"))
}

/// \[\[Get\]\] for an index: own, then up the prototype chain.
pub fn get(nogc: NoGc<'_>, o: Gc<'_, Object>, index: u32) -> Value {
    let mut cur = Some(o.cast::<Object>());
    while let Some(obj) = cur {
        if let Some(v) = get_own(nogc, obj, index) {
            return v;
        }
        cur = object::prototype(nogc, obj);
    }
    Value::UNDEFINED
}

fn sparse_find(nogc: NoGc<'_>, e: Gc<'_, Elements>, index: u32) -> Option<u32> {
    let count = nogc.read_u32(e, COUNT);
    (0..count).find(|&i| nogc.read_u32(e, DATA + ENTRY * i) == index)
}

/// The own element indices, ascending.
pub fn keys(nogc: NoGc<'_>, o: Gc<'_, Object>) -> Vec<u32> {
    let Some(e) = store(nogc, o) else {
        return Vec::new();
    };
    if is_sparse(nogc, e) {
        let count = nogc.read_u32(e, COUNT);
        let mut v: Vec<u32> = (0..count)
            .map(|i| nogc.read_u32(e, DATA + ENTRY * i))
            .collect();
        v.sort_unstable();
        v
    } else {
        (0..cap_of(nogc, e))
            .filter(|&i| nogc.read_u64(e, DATA + 8 * i) != hole())
            .collect()
    }
}

/// \[\[Set\]\] for an index with the object as receiver (OrdinarySet, all
/// elements being plain data properties): updates or adds an own
/// element; adding needs an extensible object. Arrays grow their length
/// past the index. Returns whether it succeeded.
pub fn set(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    index: u32,
    value: RootedValue<'_>,
) -> Result<bool, AllocError> {
    if index > MAX_INDEX {
        return Ok(false);
    }
    let nogc = s.nogc();
    let obj = o.get(nogc);
    if get_own(nogc, obj, index).is_none() && !object::is_extensible(nogc, obj) {
        return Ok(false);
    }
    make_room(s, o, index)?;
    let nogc = s.nogc();
    let obj = o.get(nogc);
    let e = store(nogc, obj).expect("make_room made a store");
    let (bits, referent) = value.resolve(nogc);
    if is_sparse(nogc, e) {
        let i = match sparse_find(nogc, e, index) {
            Some(i) => i,
            None => {
                let count = nogc.read_u32(e, COUNT);
                nogc.write_u32(e, DATA + ENTRY * count, index);
                nogc.write_u32(e, COUNT, count + 1);
                count
            }
        };
        nogc.write_value_bits(e, DATA + ENTRY * i + 8, bits, referent);
    } else {
        nogc.write_value_bits(e, DATA + 8 * index, bits, referent);
    }
    if is_array(nogc, obj) && index >= nogc.read_u32(e, LENGTH) {
        nogc.write_u32(e, LENGTH, index + 1);
    }
    Ok(true)
}

/// Makes sure the store can take `index` (growing it, or turning it
/// sparse with room for one more entry).
fn make_room(s: &mut Scope<'_>, o: Root<'_, Object>, index: u32) -> Result<(), AllocError> {
    let nogc = s.nogc();
    let obj = o.get(nogc);
    let old = store(nogc, obj);
    let (cap, sparse) = old.map_or((0, false), |e| (cap_of(nogc, e), is_sparse(nogc, e)));
    if sparse {
        let e = old.expect("sparse store");
        let count = nogc.read_u32(e, COUNT);
        if count < cap || sparse_find(nogc, e, index).is_some() {
            return Ok(());
        }
        return regrow(s, o, true, cap.saturating_mul(2).max(8));
    }
    if index < cap {
        return Ok(());
    }
    let near = u64::from(index) < 2 * u64::from(cap) + 8;
    if near && index < DENSE_LIMIT {
        let new_cap = (index + 1).max(cap.saturating_mul(2)).clamp(8, DENSE_LIMIT);
        regrow(s, o, false, new_cap)
    } else {
        let live = keys(nogc, obj).len() as u32;
        regrow(s, o, true, (live + 1).saturating_mul(2).max(8))
    }
}

/// Replaces the store with a new one of `new_cap` (dense or sparse),
/// copying every element and the array length.
fn regrow(
    s: &mut Scope<'_>,
    o: Root<'_, Object>,
    sparse: bool,
    new_cap: u32,
) -> Result<(), AllocError> {
    let fresh = if sparse {
        let size = DATA
            .checked_add(
                new_cap
                    .checked_mul(ENTRY)
                    .ok_or(AllocError::BadSize(u64::MAX))?,
            )
            .ok_or(AllocError::BadSize(u64::MAX))?;
        let cell = s.alloc::<Elements>(size)?;
        let nogc = s.nogc();
        let e = cell.bind(nogc);
        nogc.write_u64(
            e,
            0,
            CellKind::Elements as u64 | SPARSE | (new_cap as u64) << 32,
        );
        e.unbind()
    } else {
        alloc_dense(s, new_cap)?
    };
    let nogc = s.nogc();
    let fresh = fresh.bind(nogc);
    let obj = o.get(nogc);
    if let Some(old) = store(nogc, obj) {
        let entries: Vec<(u32, u64)> = keys(nogc, obj)
            .into_iter()
            .map(|i| {
                let v = get_own(nogc, obj, i).expect("a key");
                (i, v.to_bits())
            })
            .collect();
        // Initializing stores into a cell allocated just now need no
        // barrier referent (see `object::ensure_slot`).
        for (n, &(index, bits)) in entries.iter().enumerate() {
            if sparse {
                let at = DATA + ENTRY * n as u32;
                nogc.write_u32(fresh, at, index);
                nogc.write_value_bits(fresh, at + 8, bits, None::<Gc<'_, ()>>);
            } else {
                nogc.write_value_bits(fresh, DATA + 8 * index, bits, None::<Gc<'_, ()>>);
            }
        }
        if sparse {
            nogc.write_u32(fresh, COUNT, entries.len() as u32);
        }
        nogc.write_u32(fresh, LENGTH, nogc.read_u32(old, LENGTH));
    }
    nogc.write_ref(obj, object::ELEMENTS, Some(fresh));
    Ok(())
}

/// \[\[Delete\]\] for an index: removes the element (elements are
/// configurable). Doesn't change an array's length.
pub fn delete(nogc: NoGc<'_>, o: Gc<'_, Object>, index: u32) -> bool {
    let Some(e) = store(nogc, o) else {
        return true;
    };
    if is_sparse(nogc, e) {
        if let Some(i) = sparse_find(nogc, e, index) {
            // Move the last entry into the gap.
            let count = nogc.read_u32(e, COUNT);
            let last = count - 1;
            let idx = nogc.read_u32(e, DATA + ENTRY * last);
            let bits = nogc.read_u64(e, DATA + ENTRY * last + 8);
            nogc.write_u32(e, DATA + ENTRY * i, idx);
            nogc.write_value_bits(e, DATA + ENTRY * i + 8, bits, None::<Gc<'_, ()>>);
            nogc.write_u64(e, DATA + ENTRY * last + 8, Value::UNDEFINED.to_bits());
            nogc.write_u32(e, COUNT, last);
        }
    } else if index < cap_of(nogc, e) {
        nogc.write_u64(e, DATA + 8 * index, hole());
    }
    true
}

/// ArraySetLength for a plain new length: shrinking deletes the elements
/// at and past it. Returns `false` for a non-array.
pub fn set_length(s: &mut Scope<'_>, o: Root<'_, Object>, len: u32) -> Result<bool, AllocError> {
    let nogc = s.nogc();
    let obj = o.get(nogc);
    if !is_array(nogc, obj) {
        return Ok(false);
    }
    for i in keys(nogc, obj).into_iter().filter(|&i| i >= len) {
        delete(nogc, obj, i);
    }
    let e = store(nogc, obj).expect("arrays have a store");
    nogc.write_u32(e, LENGTH, len);
    Ok(true)
}
