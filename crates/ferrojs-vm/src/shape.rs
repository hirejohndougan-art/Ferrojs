//! Shapes (hidden classes, ARCHITECTURE.md §7.2, D-024, D-038).
//!
//! A shape is one step of a transition tree: the root shape of a tree is
//! the empty layout for one (prototype, inline slot count) pair, and each
//! child adds one property (an atom key with its attributes) in the next
//! slot. Objects built the same way share shapes, and the prototype lives
//! in the shape, so one shape check covers layout and prototype.
//!
//! Layout (40 bytes):
//!
//! ```text
//!  0  header   kind | attributes << 8 | property count << 32
//!  8  parent   shape this one extends (0 for a root)
//! 12  proto    the prototype object (0 for null)
//! 16  key      the atom this shape adds (0 for a root)
//! 20  inline   inline slot count of objects with this shape
//! 24  child    first transition out of this shape
//! 28  sibling  next transition out of this shape's parent
//! 32  table    key table (0 below `TABLE_MIN` properties)
//! ```
//!
//! A property's slot is its shape's count minus one. Transitions are a
//! linked list of children; Phase 1 keeps the whole tree alive while its
//! root lives (Phase 5 makes transitions weak). Small shapes are searched
//! by walking the parent chain; shapes with `TABLE_MIN` or more
//! properties carry a hashed key table shared along the chain (task 1.8f,
//! D-060). Dictionary shapes (task 1.8b, D-039) hash their table too.
//!
//! Every hash here is of a key atom's heap offset. That needs cells that
//! never move, which holds in Phase 1 (the root shape table relies on it
//! too); a moving collector must rehash these tables (D-060).

use ferrojs_core::gc::{Root, Scope};
use ferrojs_core::{AllocError, Gc, NoGc, Unbound};

use crate::{CellKind, JsString, Object, Tables};

/// Type marker for shape cells.
pub struct Shape;

pub(crate) const PARENT: u32 = 8;
pub(crate) const PROTO: u32 = 12;
pub(crate) const KEY: u32 = 16;
pub(crate) const INLINE: u32 = 20;
pub(crate) const CHILD: u32 = 24;
pub(crate) const SIBLING: u32 = 28;
pub(crate) const TABLE: u32 = 32;
pub(crate) const SIZE: u32 = 40;

/// The reference fields, for the tracer.
pub(crate) const REF_FIELDS: [u32; 6] = [PARENT, PROTO, KEY, CHILD, SIBLING, TABLE];

/// Shared shapes with at least this many properties have a key table.
pub const TABLE_MIN: u32 = 8;

/// Property attributes (ECMA-262 §6.1.7.1). An accessor property has
/// [`Attrs::ACCESSOR`] and never [`Attrs::WRITABLE`]; its slot holds an
/// accessor pair cell (task 1.8d, D-055).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Attrs(pub u8);

impl Attrs {
    pub const WRITABLE: Attrs = Attrs(1);
    pub const ENUMERABLE: Attrs = Attrs(2);
    pub const CONFIGURABLE: Attrs = Attrs(4);
    /// An accessor property: the slot holds its getter and setter pair.
    pub const ACCESSOR: Attrs = Attrs(8);
    /// What assignment creates: writable, enumerable, configurable.
    pub const DEFAULT: Attrs = Attrs(7);

    pub const fn contains(self, other: Attrs) -> bool {
        self.0 & other.0 == other.0
    }

    /// These attributes with `other`'s bits added.
    pub const fn with(self, other: Attrs) -> Attrs {
        Attrs(self.0 | other.0)
    }
}

fn header(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> u64 {
    let h = nogc.read_u64(shape, 0);
    debug_assert_eq!(h as u8, CellKind::Shape as u8, "not a shape cell");
    h
}

/// How many properties objects with this shape have (for a dictionary
/// shape: how many slots have been handed out).
pub fn count(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> u32 {
    (header(nogc, shape) >> 32) as u32
}

pub fn prototype<'a>(nogc: NoGc<'a>, shape: Gc<'_, Shape>) -> Option<Gc<'a, Object>> {
    nogc.read_ref(shape, PROTO)
}

pub fn inline_slots(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> u32 {
    nogc.read_u32(shape, INLINE)
}

/// The key a shared shape adds (`None` for a root or dictionary shape).
pub fn last_key<'a>(nogc: NoGc<'a>, shape: Gc<'_, Shape>) -> Option<Gc<'a, JsString>> {
    if is_dictionary(nogc, shape) {
        return None;
    }
    nogc.read_ref(shape, KEY)
}

/// The shape a shared shape extends.
pub fn parent<'a>(nogc: NoGc<'a>, shape: Gc<'_, Shape>) -> Option<Gc<'a, Shape>> {
    nogc.read_ref(shape, PARENT)
}

/// A property found in a shape: its slot and attributes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Property {
    pub slot: u32,
    pub attrs: Attrs,
}

/// Finds `key` (an atom) among the shape's properties.
pub fn lookup(nogc: NoGc<'_>, shape: Gc<'_, Shape>, key: Gc<'_, JsString>) -> Option<Property> {
    if is_dictionary(nogc, shape) {
        return dict_lookup(nogc, shape, key).map(|(_, p)| p);
    }
    if let Some(t) = nogc.read_ref::<_, KeyTable>(shape, TABLE) {
        let n = count(nogc, shape);
        let i = probe(
            nogc,
            t,
            kt_index(nogc, t),
            kt_buckets(nogc, t),
            key.offset(),
            |i| nogc.read_u32(t, KT_ENTRIES + KT_ENTRY * i),
        )?;
        // Entries past this shape's count belong to shapes further down
        // the chain that share the table.
        return (i < n).then(|| Property {
            slot: i,
            attrs: Attrs(nogc.read_u32(t, KT_ENTRIES + KT_ENTRY * i + 4) as u8),
        });
    }
    let mut s = Some(shape.cast::<Shape>());
    while let Some(cur) = s {
        // Compare offsets: keys are atoms. The header is read only on a hit.
        if nogc.read_u32(cur, KEY) == key.offset() {
            let h = header(nogc, cur);
            return Some(Property {
                slot: (h >> 32) as u32 - 1,
                attrs: Attrs((h >> 8) as u8),
            });
        }
        s = nogc.read_ref(cur, PARENT);
    }
    None
}

/// The keys of the shape's properties, in the order they were added.
pub fn keys<'a>(nogc: NoGc<'a>, shape: Gc<'_, Shape>) -> Vec<Gc<'a, JsString>> {
    if is_dictionary(nogc, shape) {
        return entries(nogc, shape)
            .into_iter()
            .filter_map(|(_, k, _, _)| k)
            .collect();
    }
    chain(nogc, shape).into_iter().map(|(k, _)| k).collect()
}

/// The keys and attributes of a shared shape's properties, in slot order.
fn chain<'a>(nogc: NoGc<'a>, shape: Gc<'_, Shape>) -> Vec<(Gc<'a, JsString>, Attrs)> {
    let n = count(nogc, shape);
    if let Some(t) = nogc.read_ref::<_, KeyTable>(shape, TABLE) {
        return (0..n)
            .map(|i| {
                let at = KT_ENTRIES + KT_ENTRY * i;
                let k = nogc.read_ref(t, at).expect("key table entries have keys");
                (k, Attrs(nogc.read_u32(t, at + 4) as u8))
            })
            .collect();
    }
    let mut out = Vec::with_capacity(n as usize);
    let mut s: Option<Gc<'_, Shape>> = Some(shape.cast());
    while let Some(cur) = s {
        if let Some(k) = nogc.read_ref(cur, KEY) {
            out.push((k, Attrs((header(nogc, cur) >> 8) as u8)));
        }
        s = nogc.read_ref(cur, PARENT);
    }
    out.reverse();
    out
}

// ---- hashed indexes (task 1.8f, D-060) ----
//
// Key tables and dictionary tables end with an open-addressing index: a
// power-of-two number of `u32` buckets, each 0 (empty) or an entry index
// plus one, probed linearly from the key's hash. Buckets are never
// cleared (a deleted dictionary entry keeps its bucket and simply no
// longer matches), and every table has at least twice as many buckets as
// entries, so probing always reaches an empty bucket.

fn mix(key: u32) -> u32 {
    let h = key.wrapping_mul(0x9E37_79B9);
    h ^ (h >> 15)
}

/// Finds the entry whose key offset is `key` through the index at
/// `index`; `key_at` reads an entry's key offset.
fn probe<T>(
    nogc: NoGc<'_>,
    t: Gc<'_, T>,
    index: u32,
    buckets: u32,
    key: u32,
    key_at: impl Fn(u32) -> u32,
) -> Option<u32> {
    debug_assert!(buckets.is_power_of_two());
    let mask = buckets - 1;
    let mut b = mix(key) & mask;
    loop {
        let e = nogc.read_u32(t, index + 4 * b);
        if e == 0 {
            return None;
        }
        if key_at(e - 1) == key {
            return Some(e - 1);
        }
        b = (b + 1) & mask;
    }
}

/// Records entry `entry` with key offset `key` in the index.
fn index_insert<T>(nogc: NoGc<'_>, t: Gc<'_, T>, index: u32, buckets: u32, key: u32, entry: u32) {
    let mask = buckets - 1;
    let mut b = mix(key) & mask;
    while nogc.read_u32(t, index + 4 * b) != 0 {
        b = (b + 1) & mask;
    }
    nogc.write_u32(t, index + 4 * b, entry + 1);
}

// ---- key tables for large shared shapes (task 1.8f, D-060) ----
//
// A key table (header: kind | capacity << 32; `len` at 8) lists the keys
// of a chain of shapes in slot order, 8-byte entries from offset 16 (key,
// attributes), then the index with `2 * capacity` buckets. A shape with a
// table uses its first `count` entries. A child appends its key to its
// parent's table when the parent's properties are the whole table and
// there is room, so a chain of shapes shares one table; otherwise (a
// branch in the tree, or a full table) it copies its parent's entries
// into a new table twice the size.

/// Type marker for key tables.
pub struct KeyTable;

pub(crate) const KT_LEN: u32 = 8;
pub(crate) const KT_ENTRIES: u32 = 16;
pub(crate) const KT_ENTRY: u32 = 8;
const KT_FIRST: u32 = 16;

pub(crate) fn kt_cap_len(cage: &ferrojs_core::Cage, table: u32) -> (u32, u32) {
    (
        (cage.read_u64(table) >> 32) as u32,
        cage.read_u32(table + KT_LEN),
    )
}

/// Bytes a key table with room for `cap` entries takes.
pub(crate) fn kt_size(cap: u32) -> Option<u32> {
    KT_ENTRIES.checked_add(cap.checked_mul(KT_ENTRY + 8)?)
}

fn kt_cap(nogc: NoGc<'_>, t: Gc<'_, KeyTable>) -> u32 {
    (nogc.read_u64(t, 0) >> 32) as u32
}

fn kt_len(nogc: NoGc<'_>, t: Gc<'_, KeyTable>) -> u32 {
    nogc.read_u32(t, KT_LEN)
}

fn kt_index(nogc: NoGc<'_>, t: Gc<'_, KeyTable>) -> u32 {
    KT_ENTRIES + KT_ENTRY * kt_cap(nogc, t)
}

fn kt_buckets(nogc: NoGc<'_>, t: Gc<'_, KeyTable>) -> u32 {
    2 * kt_cap(nogc, t)
}

/// Appends `key` with `attrs` to a key table that has room.
fn kt_push(nogc: NoGc<'_>, t: Gc<'_, KeyTable>, key: Gc<'_, JsString>, attrs: Attrs) {
    let len = kt_len(nogc, t);
    debug_assert!(len < kt_cap(nogc, t), "key table full");
    let at = KT_ENTRIES + KT_ENTRY * len;
    nogc.write_ref(t, at, Some(key));
    nogc.write_u32(t, at + 4, u32::from(attrs.0));
    index_insert(
        nogc,
        t,
        kt_index(nogc, t),
        kt_buckets(nogc, t),
        key.offset(),
        len,
    );
    nogc.write_u32(t, KT_LEN, len + 1);
}

/// The root shape for objects with prototype `proto` and `inline` inline
/// slots, made on first use.
pub fn root_shape(
    s: &mut Scope<'_>,
    t: &mut Tables,
    proto: Option<Root<'_, Object>>,
    inline: u32,
) -> Result<Unbound<Shape>, AllocError> {
    let proto_offset = proto.map_or(0, |p| p.get(s.nogc()).offset());
    if let Some(g) = t.root_shapes.get(&(proto_offset, inline)) {
        return Ok(g.get(s.nogc()).unbind());
    }
    let cell = s.alloc::<Shape>(SIZE)?;
    let nogc = s.nogc();
    let g = cell.bind(nogc);
    nogc.write_u64(g, 0, CellKind::Shape as u64);
    nogc.write_ref(g, PROTO, proto.map(|p| p.get(nogc)));
    nogc.write_u32(g, INLINE, inline);
    // The prototype can't have moved (Phase 1 doesn't move cells), so its
    // offset stays a valid key while this root shape keeps it alive.
    t.root_shapes.insert((proto_offset, inline), nogc.global(g));
    Ok(g.unbind())
}

/// The shape that adds `key` with `attrs` to `shape`: an existing
/// transition, or a new one.
pub fn add_property(
    s: &mut Scope<'_>,
    shape: Root<'_, Shape>,
    key: Root<'_, JsString>,
    attrs: Attrs,
) -> Result<Unbound<Shape>, AllocError> {
    {
        let nogc = s.nogc();
        let (parent, key) = (shape.get(nogc), key.get(nogc));
        let mut child: Option<Gc<'_, Shape>> = nogc.read_ref(parent, CHILD);
        while let Some(c) = child {
            let k: Option<Gc<'_, JsString>> = nogc.read_ref(c, KEY);
            let a = Attrs((header(nogc, c) >> 8) as u8);
            if k.map(|k| k.offset()) == Some(key.offset()) && a == attrs {
                return Ok(c.unbind());
            }
            child = nogc.read_ref(c, SIBLING);
        }
    }
    // The key table, if the new shape gets one: the parent's, when the new
    // key can go at its end, else a fresh copy (allocated and filled
    // before the shape, so nothing changes if either allocation fails).
    let new_count = count(s.nogc(), shape.get(s.nogc())) + 1;
    let mut shared: Option<Root<'_, KeyTable>> = None;
    let mut fresh: Option<Root<'_, KeyTable>> = None;
    if new_count >= TABLE_MIN {
        let nogc = s.nogc();
        let parent = shape.get(nogc);
        match nogc.read_ref::<_, KeyTable>(parent, TABLE) {
            Some(t) if kt_len(nogc, t) == new_count - 1 && kt_cap(nogc, t) >= new_count => {
                shared = Some(s.root(t));
            }
            _ => {
                let cap = new_count.next_power_of_two().max(KT_FIRST);
                let size = kt_size(cap).ok_or(AllocError::BadSize(u64::MAX))?;
                let cell = s.alloc::<KeyTable>(size)?;
                let nogc = s.nogc();
                let t = cell.bind(nogc);
                nogc.write_u64(t, 0, CellKind::KeyTable as u64 | (cap as u64) << 32);
                for (k, a) in chain(nogc, shape.get(nogc)) {
                    kt_push(nogc, t, k, a);
                }
                fresh = Some(s.root(t));
            }
        }
    }
    let cell = s.alloc::<Shape>(SIZE)?;
    let nogc = s.nogc();
    let g = cell.bind(nogc);
    let parent = shape.get(nogc);
    let count = count(nogc, parent) + 1;
    debug_assert_eq!(count, new_count);
    if let Some(t) = shared.or(fresh) {
        let t = t.get(nogc);
        kt_push(nogc, t, key.get(nogc), attrs);
        nogc.write_ref(g, TABLE, Some(t));
    }
    nogc.write_u64(
        g,
        0,
        CellKind::Shape as u64 | (attrs.0 as u64) << 8 | (count as u64) << 32,
    );
    nogc.write_ref(g, PARENT, Some(parent));
    nogc.write_ref(g, PROTO, prototype(nogc, parent));
    nogc.write_ref(g, KEY, Some(key.get(nogc)));
    nogc.write_u32(g, INLINE, inline_slots(nogc, parent));
    // Link in front of the parent's transitions.
    nogc.write_ref(g, SIBLING, nogc.read_ref::<_, Shape>(parent, CHILD));
    nogc.write_ref(parent, CHILD, Some(g));
    Ok(g.unbind())
}

// ---- dictionary mode (task 1.8b, D-039) ----
//
// A dictionary shape belongs to one object. Its header has the `DICT`
// flag (and maybe `NOT_EXTENSIBLE`) and its count is the next fresh slot;
// `KEY` refers to a `Dict` table instead of a key; parent and transitions
// are empty. The table (header: kind | capacity << 32; `len` at 8; at 12
// the number of deleted entries still holding a free slot) holds 16-byte
// entries from offset 16 in insertion order: key, attributes, slot, then
// the hashed index (`dict_buckets(capacity)` buckets, task 1.8f). A
// deleted entry has key 0 and keeps its slot for reuse until a new
// property takes it (then the slot field becomes `NO_SLOT`).

/// Type marker for dictionary tables.
pub struct Dict;

/// Shape flag: a dictionary shape.
const DICT: u64 = 1 << 16;
/// Shape flag: objects with this shape can't get new properties.
const NOT_EXTENSIBLE: u64 = 1 << 17;
pub(crate) const DICT_LEN: u32 = 8;
/// How many deleted entries still hold a free slot.
const DICT_FREE: u32 = 12;
pub(crate) const DICT_ENTRIES: u32 = 16;
const ENTRY: u32 = 16;

/// The number of index buckets of a dictionary table with room for `cap`
/// entries.
pub(crate) fn dict_buckets(cap: u32) -> Option<u32> {
    cap.checked_mul(2)?.checked_next_power_of_two()
}

/// Bytes a dictionary table with room for `cap` entries takes.
pub(crate) fn dict_size(cap: u32) -> Option<u32> {
    DICT_ENTRIES
        .checked_add(cap.checked_mul(ENTRY)?)?
        .checked_add(dict_buckets(cap)?.checked_mul(4)?)
}

fn dict_cap(nogc: NoGc<'_>, t: Gc<'_, Dict>) -> u32 {
    (nogc.read_u64(t, 0) >> 32) as u32
}

/// Where a dictionary table's index starts, and its bucket count.
fn dict_index(nogc: NoGc<'_>, t: Gc<'_, Dict>) -> (u32, u32) {
    let cap = dict_cap(nogc, t);
    let buckets = dict_buckets(cap).expect("a table this size was allocated");
    (DICT_ENTRIES + ENTRY * cap, buckets)
}
const NO_SLOT: u32 = u32::MAX;

/// Whether `shape` is a dictionary shape.
pub fn is_dictionary(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> bool {
    header(nogc, shape) & DICT != 0
}

/// Whether objects with this shape can get new properties.
pub fn is_extensible(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> bool {
    header(nogc, shape) & NOT_EXTENSIBLE == 0
}

pub(crate) fn dict_len(cage: &ferrojs_core::Cage, table: u32) -> u32 {
    cage.read_u32(table + DICT_LEN)
}

fn table<'a>(nogc: NoGc<'a>, shape: Gc<'_, Shape>) -> Gc<'a, Dict> {
    debug_assert!(is_dictionary(nogc, shape));
    nogc.read_ref(shape, KEY)
        .expect("dictionary shapes have a table")
}

/// The dictionary entries: (index, key or None if deleted, attrs, slot).
fn entries<'a>(
    nogc: NoGc<'a>,
    shape: Gc<'_, Shape>,
) -> Vec<(u32, Option<Gc<'a, JsString>>, Attrs, u32)> {
    let t = table(nogc, shape);
    let len = nogc.read_u32(t, DICT_LEN);
    (0..len)
        .map(|i| {
            let at = DICT_ENTRIES + ENTRY * i;
            (
                i,
                nogc.read_ref(t, at),
                Attrs(nogc.read_u32(t, at + 4) as u8),
                nogc.read_u32(t, at + 8),
            )
        })
        .collect()
}

fn dict_lookup(
    nogc: NoGc<'_>,
    shape: Gc<'_, Shape>,
    key: Gc<'_, JsString>,
) -> Option<(u32, Property)> {
    let t = table(nogc, shape);
    let (index, buckets) = dict_index(nogc, t);
    // Deleted entries have key 0, which no atom's offset is.
    let i = probe(nogc, t, index, buckets, key.offset(), |i| {
        nogc.read_u32(t, DICT_ENTRIES + ENTRY * i)
    })?;
    let at = DICT_ENTRIES + ENTRY * i;
    let attrs = Attrs(nogc.read_u32(t, at + 4) as u8);
    let slot = nogc.read_u32(t, at + 8);
    Some((i, Property { slot, attrs }))
}

/// The first deleted entry still holding a free slot: (index, slot).
fn first_free(nogc: NoGc<'_>, t: Gc<'_, Dict>) -> Option<(u32, u32)> {
    if nogc.read_u32(t, DICT_FREE) == 0 {
        return None;
    }
    let len = nogc.read_u32(t, DICT_LEN);
    (0..len).find_map(|i| {
        let at = DICT_ENTRIES + ENTRY * i;
        let slot = nogc.read_u32(t, at + 8);
        (nogc.read_u32(t, at) == 0 && slot != NO_SLOT).then_some((i, slot))
    })
}

/// The slot [`dict_add`] would give a new property: the first free slot
/// of a deleted entry, else the next fresh one. Changes nothing, so the
/// caller can make storage for it first.
pub fn dict_next_slot(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> u32 {
    first_free(nogc, table(nogc, shape)).map_or_else(|| count(nogc, shape), |(_, slot)| slot)
}

/// Allocates a dictionary table with room for `cap` entries and copies
/// `rows` (key, attrs, slot) into it.
fn new_table(
    s: &mut Scope<'_>,
    cap: u32,
    rows: &[(Root<'_, JsString>, Attrs, u32)],
    tombstones: &[u32],
) -> Result<Unbound<Dict>, AllocError> {
    let size = dict_size(cap).ok_or(AllocError::BadSize(u64::MAX))?;
    let cell = s.alloc::<Dict>(size)?;
    let nogc = s.nogc();
    let t = cell.bind(nogc);
    nogc.write_u64(t, 0, CellKind::Dict as u64 | (cap as u64) << 32);
    let (index, buckets) = dict_index(nogc, t);
    let mut i = 0;
    for &(k, attrs, slot) in rows {
        let at = DICT_ENTRIES + ENTRY * i;
        let k = k.get(nogc);
        nogc.write_ref(t, at, Some(k));
        nogc.write_u32(t, at + 4, u32::from(attrs.0));
        nogc.write_u32(t, at + 8, slot);
        index_insert(nogc, t, index, buckets, k.offset(), i);
        i += 1;
    }
    // Free slots survive compaction as deleted entries.
    for &slot in tombstones {
        let at = DICT_ENTRIES + ENTRY * i;
        nogc.write_u32(t, at + 8, slot);
        i += 1;
    }
    nogc.write_u32(t, DICT_LEN, i);
    nogc.write_u32(t, DICT_FREE, tombstones.len() as u32);
    Ok(t.unbind())
}

/// A new dictionary shape describing exactly the properties of the shared
/// shape `shape` (same prototype, inline count and extensibility).
pub fn to_dictionary(
    s: &mut Scope<'_>,
    shape: Root<'_, Shape>,
) -> Result<Unbound<Shape>, AllocError> {
    let nogc = s.nogc();
    let sh = shape.get(nogc);
    assert!(!is_dictionary(nogc, sh), "already a dictionary shape");
    // A shared shape's slots are its properties' positions.
    let rows: Vec<(Gc<'_, JsString>, Attrs)> = chain(nogc, sh);
    let rows: Vec<_> = rows
        .into_iter()
        .zip(0..)
        .map(|((k, a), slot)| (s.root(k), a, slot))
        .collect();
    let next = count(nogc, sh);
    let cap = (rows.len() as u32 * 2).max(8);
    let t = new_table(s, cap, &rows, &[])?.bind(s.nogc());
    let t = s.root(t);
    let cell = s.alloc::<Shape>(SIZE)?;
    let nogc = s.nogc();
    let g = cell.bind(nogc);
    let old = shape.get(nogc);
    let flags = DICT | (header(nogc, old) & NOT_EXTENSIBLE);
    nogc.write_u64(g, 0, CellKind::Shape as u64 | flags | (next as u64) << 32);
    nogc.write_ref(g, PROTO, prototype(nogc, old));
    nogc.write_ref(g, KEY, Some(t.get(nogc)));
    nogc.write_u32(g, INLINE, inline_slots(nogc, old));
    Ok(g.unbind())
}

/// Adds `key` to a dictionary shape and returns the slot it got: the one
/// [`dict_next_slot`] names (callers make its storage first).
pub fn dict_add(
    s: &mut Scope<'_>,
    shape: Root<'_, Shape>,
    key: Root<'_, JsString>,
    attrs: Attrs,
) -> Result<u32, AllocError> {
    let nogc = s.nogc();
    let sh = shape.get(nogc);
    let t = table(nogc, sh);
    let cap = dict_cap(nogc, t);
    let len = nogc.read_u32(t, DICT_LEN);
    if len == cap {
        let all = entries(nogc, sh);
        // Full: compact (dropping spent tombstones, keeping free slots)
        // into a table with room to spare.
        let live: Vec<_> = all
            .iter()
            .filter_map(|&(_, k, a, slot)| k.map(|k| (k, a, slot)))
            .collect();
        let free_slots: Vec<u32> = all
            .iter()
            .filter(|(_, k, _, slot)| k.is_none() && *slot != NO_SLOT)
            .map(|&(_, _, _, slot)| slot)
            .collect();
        let live: Vec<_> = live
            .into_iter()
            .map(|(k, a, slot)| (s.root(k), a, slot))
            .collect();
        let new_cap = ((live.len() + free_slots.len()) as u32 * 2).max(8).max(cap);
        let fresh = new_table(s, new_cap, &live, &free_slots)?.bind(s.nogc());
        let nogc = s.nogc();
        nogc.write_ref(shape.get(nogc), KEY, Some(fresh));
        return dict_add(s, shape, key, attrs);
    }
    let slot = match first_free(nogc, t) {
        Some((i, slot)) => {
            nogc.write_u32(t, DICT_ENTRIES + ENTRY * i + 8, NO_SLOT);
            let free = nogc.read_u32(t, DICT_FREE);
            nogc.write_u32(t, DICT_FREE, free - 1);
            slot
        }
        None => {
            let next = count(nogc, sh);
            let h = header(nogc, sh) & !(u64::from(u32::MAX) << 32);
            nogc.write_u64(sh, 0, h | ((next + 1) as u64) << 32);
            next
        }
    };
    let at = DICT_ENTRIES + ENTRY * len;
    let key = key.get(nogc);
    nogc.write_ref(t, at, Some(key));
    nogc.write_u32(t, at + 4, u32::from(attrs.0));
    nogc.write_u32(t, at + 8, slot);
    let (index, buckets) = dict_index(nogc, t);
    index_insert(nogc, t, index, buckets, key.offset(), len);
    nogc.write_u32(t, DICT_LEN, len + 1);
    Ok(slot)
}

/// Removes `key` from a dictionary shape; returns its slot.
pub fn dict_remove(nogc: NoGc<'_>, shape: Gc<'_, Shape>, key: Gc<'_, JsString>) -> Option<u32> {
    let (i, p) = dict_lookup(nogc, shape, key)?;
    let t = table(nogc, shape);
    nogc.write_ref(t, DICT_ENTRIES + ENTRY * i, None::<Gc<'_, JsString>>);
    // A live entry always holds its slot, so it is free for reuse now.
    debug_assert_ne!(p.slot, NO_SLOT);
    let free = nogc.read_u32(t, DICT_FREE);
    nogc.write_u32(t, DICT_FREE, free + 1);
    Some(p.slot)
}

/// Changes the attributes of `key` in a dictionary shape.
pub fn dict_set_attrs(
    nogc: NoGc<'_>,
    shape: Gc<'_, Shape>,
    key: Gc<'_, JsString>,
    attrs: Attrs,
) -> bool {
    let Some((i, _)) = dict_lookup(nogc, shape, key) else {
        return false;
    };
    let t = table(nogc, shape);
    nogc.write_u32(t, DICT_ENTRIES + ENTRY * i + 4, u32::from(attrs.0));
    true
}

/// Marks a dictionary shape non-extensible.
pub fn dict_prevent_extensions(nogc: NoGc<'_>, shape: Gc<'_, Shape>) {
    debug_assert!(is_dictionary(nogc, shape));
    let h = header(nogc, shape);
    nogc.write_u64(shape, 0, h | NOT_EXTENSIBLE);
}
