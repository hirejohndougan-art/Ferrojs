//! Shapes (hidden classes, ARCHITECTURE.md §7.2, D-024, D-038).
//!
//! A shape is one step of a transition tree: the root shape of a tree is
//! the empty layout for one (prototype, inline slot count) pair, and each
//! child adds one property (an atom key with its attributes) in the next
//! slot. Objects built the same way share shapes, and the prototype lives
//! in the shape, so one shape check covers layout and prototype.
//!
//! Layout (32 bytes):
//!
//! ```text
//!  0  header   kind | attributes << 8 | property count << 32
//!  8  parent   shape this one extends (0 for a root)
//! 12  proto    the prototype object (0 for null)
//! 16  key      the atom this shape adds (0 for a root)
//! 20  inline   inline slot count of objects with this shape
//! 24  child    first transition out of this shape
//! 28  sibling  next transition out of this shape's parent
//! ```
//!
//! A property's slot is its shape's count minus one. Transitions are a
//! linked list of children; Phase 1 keeps the whole tree alive while its
//! root lives (Phase 5 makes transitions weak). Lookup walks the parent
//! chain; the lazy key table for large shapes and dictionary mode come
//! with task 1.8b.

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
const SIZE: u32 = 32;

/// The reference fields, for the tracer.
pub(crate) const REF_FIELDS: [u32; 5] = [PARENT, PROTO, KEY, CHILD, SIBLING];

/// Property attributes (ECMA-262 §6.1.7.1, data properties only so far).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Attrs(pub u8);

impl Attrs {
    pub const WRITABLE: Attrs = Attrs(1);
    pub const ENUMERABLE: Attrs = Attrs(2);
    pub const CONFIGURABLE: Attrs = Attrs(4);
    /// What assignment creates: writable, enumerable, configurable.
    pub const DEFAULT: Attrs = Attrs(7);

    pub const fn contains(self, other: Attrs) -> bool {
        self.0 & other.0 == other.0
    }
}

fn header(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> u64 {
    let h = nogc.read_u64(shape, 0);
    debug_assert_eq!(h as u8, CellKind::Shape as u8, "not a shape cell");
    h
}

/// How many properties objects with this shape have.
pub fn count(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> u32 {
    (header(nogc, shape) >> 32) as u32
}

pub fn prototype<'a>(nogc: NoGc<'a>, shape: Gc<'_, Shape>) -> Option<Gc<'a, Object>> {
    nogc.read_ref(shape, PROTO)
}

pub fn inline_slots(nogc: NoGc<'_>, shape: Gc<'_, Shape>) -> u32 {
    nogc.read_u32(shape, INLINE)
}

/// A property found in a shape: its slot and attributes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Property {
    pub slot: u32,
    pub attrs: Attrs,
}

/// Finds `key` (an atom) among the shape's properties.
pub fn lookup(nogc: NoGc<'_>, shape: Gc<'_, Shape>, key: Gc<'_, JsString>) -> Option<Property> {
    let mut s = Some(shape.cast::<Shape>());
    while let Some(cur) = s {
        let h = header(nogc, cur);
        let k: Option<Gc<'_, JsString>> = nogc.read_ref(cur, KEY);
        if k.map(|k| k.offset()) == Some(key.offset()) {
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
    let mut out = Vec::new();
    let mut s: Option<Gc<'_, Shape>> = Some(shape.cast());
    while let Some(cur) = s {
        if let Some(k) = nogc.read_ref(cur, KEY) {
            out.push(k);
        }
        s = nogc.read_ref(cur, PARENT);
    }
    out.reverse();
    out
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
    let cell = s.alloc::<Shape>(SIZE)?;
    let nogc = s.nogc();
    let g = cell.bind(nogc);
    let parent = shape.get(nogc);
    let count = count(nogc, parent) + 1;
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
