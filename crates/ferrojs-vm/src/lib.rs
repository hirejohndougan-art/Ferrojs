//! The FerroJS runtime (ARCHITECTURE.md §5, §7).
//!
//! Task 1.8 (D-038) starts it with the heap cell layouts the engine
//! defines on top of `ferrojs-core`: strings and interned atoms
//! ([`string`]), shapes ([`shape`]) and ordinary objects ([`object`]),
//! plus the [`Tracer`] that tells the collector where each kind keeps its
//! references. The interpreter (1.7) and built-ins (1.13) join it later.
//!
//! Every cell starts with an 8-byte header whose low byte is its
//! [`CellKind`]. Cells hold only references (32-bit offsets) and value
//! bits, never Rust pointers (D-009).
//!
//! Engine-wide tables that point into the heap (the atom table, root
//! shapes) live in [`Tables`]; [`Vm::with`] opens a root scope and hands
//! out both:
//!
//! ```
//! use ferrojs_vm::{object, string, RootedValue, Vm};
//! use ferrojs_value::Value;
//!
//! let mut vm = Vm::new(Default::default()).unwrap();
//! let got = vm.with(|s, t| {
//!     let o = object::new_object(s, t, None, object::DEFAULT_INLINE).unwrap().bind(s.nogc());
//!     let o = s.root(o);
//!     let k = string::atom(s, t, "answer").unwrap().bind(s.nogc());
//!     let k = s.root(k);
//!     object::set(s, o, k, RootedValue::Primitive(Value::from_i32(42))).unwrap();
//!     object::get(s.nogc(), o.get(s.nogc()), k.get(s.nogc()))
//! });
//! assert_eq!(got, Value::from_i32(42));
//! ```

use std::collections::HashMap;

use ferrojs_core::gc::{Cx, CxOptions, Global, Root, Scope};
use ferrojs_core::heap::HeapOptions;
use ferrojs_core::{Cage, CageError, Gc, Heap, NoGc, Trace};
use ferrojs_value::{Kind, Value};

pub mod elements;
pub mod host;
pub mod object;
pub mod shape;
pub mod string;

pub use object::Object;
pub use shape::Shape;
pub use string::JsString;

/// The kind byte at the start of every engine cell. 0 is never used (a
/// zeroed cell has no kind yet) and 0xff is the collector's freed marker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum CellKind {
    String = 1,
    Shape = 2,
    Object = 3,
    /// An object's out-of-line named-property slots.
    Slots = 4,
    /// A dictionary-mode shape's property table.
    Dict = 5,
    /// An object's indexed properties.
    Elements = 6,
}

impl CellKind {
    pub fn from_byte(b: u8) -> Option<CellKind> {
        match b {
            1 => Some(CellKind::String),
            2 => Some(CellKind::Shape),
            3 => Some(CellKind::Object),
            4 => Some(CellKind::Slots),
            5 => Some(CellKind::Dict),
            6 => Some(CellKind::Elements),
            _ => None,
        }
    }
}

/// The heap reference inside value bits, if any.
pub fn value_ref(bits: u64) -> Option<u32> {
    match Value::from_bits_checked(bits)?.kind() {
        Kind::Object(r) | Kind::String(r) | Kind::Other(_, r) => Some(r.0),
        _ => None,
    }
}

/// Traces every engine cell kind (ARCHITECTURE.md §6.4).
#[derive(Debug, Default)]
pub struct Tracer;

impl Trace for Tracer {
    fn trace(&self, cage: &Cage, cell: u32, size: u32, edges: &mut Vec<u32>) {
        let header = cage.read_u64(cell);
        let kind = CellKind::from_byte(header as u8)
            .unwrap_or_else(|| panic!("cell {cell:#x} has unknown kind {:#x}", header as u8));
        // Only elements stores hold the hole marker.
        let holes_ok = kind == CellKind::Elements;
        let value_at = |field: u32, edges: &mut Vec<u32>| {
            let bits = cage.read_u64(cell + field);
            debug_assert!(
                Value::from_bits_checked(bits).is_some()
                    || (holes_ok && bits == Value::UNINITIALIZED.to_bits()),
                "cell {cell:#x} holds invalid value bits {bits:#x} at {field:#x}"
            );
            if let Some(r) = value_ref(bits) {
                edges.push(r);
            }
        };
        match kind {
            CellKind::String => {}
            CellKind::Shape => {
                for field in shape::REF_FIELDS {
                    edges.push(cage.read_u32(cell + field));
                }
            }
            CellKind::Object => {
                edges.push((header >> 32) as u32);
                edges.push(cage.read_u32(cell + object::ELEMENTS));
                edges.push(cage.read_u32(cell + object::SLOTS));
                let inline = object::inline_count(header);
                assert!(
                    object::INLINE + 8 * inline <= size,
                    "object larger than its cell"
                );
                for i in 0..inline {
                    value_at(object::INLINE + 8 * i, edges);
                }
            }
            CellKind::Dict => {
                let cap = (header >> 32) as u32;
                let len = shape::dict_len(cage, cell);
                assert!(
                    len <= cap && shape::DICT_ENTRIES + 16 * cap <= size,
                    "bad table"
                );
                for i in 0..len {
                    edges.push(cage.read_u32(cell + shape::DICT_ENTRIES + 16 * i));
                }
            }
            CellKind::Elements => {
                let cap = (header >> 32) as u32;
                if header & elements::SPARSE != 0 {
                    let count = cage.read_u32(cell + elements::COUNT);
                    assert!(
                        count <= cap && elements::DATA + 16 * cap <= size,
                        "bad elements"
                    );
                    for i in 0..count {
                        value_at(elements::DATA + 16 * i + 8, edges);
                    }
                } else {
                    assert!(elements::DATA + 8 * cap <= size, "bad elements");
                    for i in 0..cap {
                        value_at(elements::DATA + 8 * i, edges);
                    }
                }
            }
            CellKind::Slots => {
                let cap = (header >> 32) as u32;
                assert!(8 + 8 * cap <= size, "slots larger than their cell");
                for i in 0..cap {
                    value_at(8 + 8 * i, edges);
                }
            }
        }
    }
}

/// Engine-wide tables pointing into the heap. Phase 1 keeps both strong:
/// atoms and root shapes live as long as the engine (D-038).
#[derive(Default)]
pub struct Tables {
    pub(crate) atoms: HashMap<Vec<u16>, Global<JsString>>,
    /// Root shapes by (prototype offset or 0, inline slot count).
    pub(crate) root_shapes: HashMap<(u32, u32), Global<Shape>>,
}

impl Tables {
    /// How many atoms are interned.
    pub fn atom_count(&self) -> usize {
        self.atoms.len()
    }
}

/// Options for a new [`Vm`].
#[derive(Clone, Copy, Debug)]
pub struct VmOptions {
    /// Heap size in bytes (rounded to chunks by the cage).
    pub heap_size: u64,
    pub heap: HeapOptions,
    pub cx: CxOptions,
}

impl Default for VmOptions {
    fn default() -> VmOptions {
        VmOptions {
            heap_size: 64 * ferrojs_core::cage::CHUNK_SIZE as u64,
            heap: HeapOptions::default(),
            cx: CxOptions::default(),
        }
    }
}

/// The runtime: the GC context, the engine tables and the embedder's
/// host hooks.
pub struct Vm {
    pub cx: Cx,
    pub tables: Tables,
    /// Randomness, clocks and job order (D-008, D-041).
    pub host: Box<dyn host::Host>,
}

impl Vm {
    /// A runtime with the default host ([`host::SystemHost`]).
    pub fn new(options: VmOptions) -> Result<Vm, CageError> {
        Vm::with_host(options, Box::new(host::SystemHost::new()))
    }

    /// A runtime with the embedder's host hooks.
    pub fn with_host(options: VmOptions, host: Box<dyn host::Host>) -> Result<Vm, CageError> {
        let cage = Cage::new(options.heap_size)?;
        let heap = Heap::from_cage(cage, options.heap);
        Ok(Vm {
            cx: Cx::new(heap, Box::new(Tracer), options.cx),
            tables: Tables::default(),
            host,
        })
    }

    /// Runs `f` in a new root scope with the engine tables.
    pub fn with<R>(&mut self, f: impl for<'s> FnOnce(&mut Scope<'s>, &mut Tables) -> R) -> R {
        let Vm { cx, tables, .. } = self;
        cx.scope(|s| f(s, tables))
    }
}

/// A value to store that survives allocation: primitives as they are,
/// references through a root.
#[derive(Clone, Copy)]
pub enum RootedValue<'s> {
    Primitive(Value),
    Object(Root<'s, Object>),
    String(Root<'s, JsString>),
}

impl RootedValue<'_> {
    /// The value's bits and the cell it refers to, if any (for the write
    /// barrier).
    pub(crate) fn resolve<'a>(self, nogc: NoGc<'a>) -> (u64, Option<Gc<'a, ()>>) {
        match self {
            RootedValue::Primitive(v) => {
                debug_assert!(
                    value_ref(v.to_bits()).is_none(),
                    "use a root for references"
                );
                (v.to_bits(), None)
            }
            RootedValue::Object(r) => {
                let g = r.get(nogc);
                (
                    Value::object(ferrojs_value::HeapRef(g.offset())).to_bits(),
                    Some(g.cast()),
                )
            }
            RootedValue::String(r) => {
                let g = r.get(nogc);
                (
                    Value::string(ferrojs_value::HeapRef(g.offset())).to_bits(),
                    Some(g.cast()),
                )
            }
        }
    }
}
