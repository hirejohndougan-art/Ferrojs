//! Branded GC handles and the engine context (D-023, D-030).
//!
//! The collector (`heap`) may run at any allocation, and later at any call
//! that can run JavaScript. Every such function takes `&mut Cx`. The
//! borrow checker then enforces the rooting rules:
//!
//! - [`Cx::nogc`] gives a [`NoGc<'a>`] token that borrows the context
//!   shared. Unrooted handles ([`Gc<'a, T>`]) carry its lifetime, so while
//!   one is alive nothing that takes `&mut Cx` can be called: an unrooted
//!   handle can't be held across a collection.
//! - Functions that may collect return [`Unbound<T>`], which can only be
//!   turned into a handle with [`Unbound::bind`].
//! - To keep a handle across a collection, root it: [`Scope::root`] pushes
//!   it on the handle stack inside [`Cx::scope`], and leaving the scope
//!   pops it. [`Global`] is a root that outlives scopes.
//! - Reads and writes of cell memory go through the token ([`NoGc`]) and
//!   never allocate. [`NoGc::write_ref`] and [`NoGc::write_value_bits`]
//!   run the write barrier (empty in Phase 1); every store of a reference
//!   into a cell must use one of them.
//! - GC stress mode ([`CxOptions::gc_stress`], or `FERROJS_GC_STRESS=1`)
//!   collects before every allocation.
//!
//! The known hole: an [`Unbound`] (or [`Gc::unbind`]) kept in a variable
//! across a collection. With the heap verifier on, binding one that points
//! at a freed cell panics, and stress mode makes that happen at once.
//!
//! A handle must only be used with the context that made it. Roots and
//! globals check this at run time; plain handles don't (they are four
//! bytes), and a mixed-up one is caught by the heap's checked accessors.
//! None of this code is `unsafe`: a rooting mistake can read the wrong
//! cell or panic, never break memory safety.
//!
//! ```
//! use ferrojs_core::gc::{Cx, CxOptions, NoEdges};
//! use ferrojs_core::Heap;
//!
//! struct Pair; // a cell type marker
//!
//! let heap = Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
//! let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: true });
//! let sum = cx.scope(|s| {
//!     let a = s.alloc::<Pair>(16).unwrap().bind(s.nogc());
//!     s.nogc().write_u64(a, 8, 40);
//!     let a = s.root(a);
//!     // `a` (the unrooted handle) is dead here; the root survives this
//!     // allocation, which collects first in stress mode.
//!     let b = s.alloc::<Pair>(16).unwrap().bind(s.nogc());
//!     s.nogc().write_u64(b, 8, 2);
//!     let a = a.get(s.nogc());
//!     s.nogc().read_u64(a, 8) + s.nogc().read_u64(b, 8)
//! });
//! assert_eq!(sum, 42);
//! ```

use std::cell::RefCell;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cage::Cage;
use crate::heap::{AllocError, CollectStats, Heap, Trace};

/// Context switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CxOptions {
    /// Collect before every allocation (ARCHITECTURE §6.5).
    pub gc_stress: bool,
}

impl CxOptions {
    /// Stress mode on when the environment variable `FERROJS_GC_STRESS`
    /// is `1`.
    pub fn from_env() -> CxOptions {
        CxOptions {
            gc_stress: std::env::var_os("FERROJS_GC_STRESS").is_some_and(|v| v == "1"),
        }
    }
}

impl Default for CxOptions {
    /// Same as [`CxOptions::from_env`].
    fn default() -> CxOptions {
        CxOptions::from_env()
    }
}

/// A [`Trace`] for heaps whose cells hold no references.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEdges;

impl Trace for NoEdges {
    fn trace(&self, _: &Cage, _: u32, _: u32, _: &mut Vec<u32>) {}
}

/// A cell type marker: covariant, and `Send`/`Sync`-neutral whatever `T`.
type TypeMarker<T> = PhantomData<fn() -> T>;

/// Makes `'s` invariant, so a root can't be passed off as one of another
/// scope.
type Brand<'s> = PhantomData<fn(&'s ()) -> &'s ()>;

/// Distinguishes contexts, so roots can check they are used with theirs.
static NEXT_CX_ID: AtomicU64 = AtomicU64::new(1);

/// The global root table, shared with every [`Global`] so dropping one
/// can free its slot. Free slots hold 0 (null), which the collector skips.
#[derive(Debug, Default)]
struct GlobalSlots {
    cells: Vec<u32>,
    free: Vec<u32>,
}

/// The engine context: the heap, its roots and the cell tracer.
///
/// Not `Send` or `Sync` (it owns a [`Heap`]). The tracer must not create
/// or drop roots: it runs while the root tables are borrowed.
pub struct Cx {
    heap: Heap,
    tracer: Box<dyn Trace>,
    /// The handle stack: one cell offset per scoped [`Root`].
    handles: RefCell<Vec<u32>>,
    globals: Rc<RefCell<GlobalSlots>>,
    options: CxOptions,
    id: u64,
    collections: u64,
}

impl fmt::Debug for Cx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cx")
            .field("id", &self.id)
            .field("options", &self.options)
            .field("handles", &self.handles.borrow().len())
            .field("globals", &self.global_count())
            .field("collections", &self.collections)
            .field("heap", &self.heap)
            .finish_non_exhaustive()
    }
}

impl Cx {
    /// A context over `heap`, whose cells `tracer` knows how to trace.
    pub fn new(heap: Heap, tracer: Box<dyn Trace>, options: CxOptions) -> Cx {
        Cx {
            heap,
            tracer,
            handles: RefCell::new(Vec::new()),
            globals: Rc::new(RefCell::new(GlobalSlots::default())),
            options,
            id: NEXT_CX_ID.fetch_add(1, Ordering::Relaxed),
            collections: 0,
        }
    }

    /// The switches in force.
    pub fn options(&self) -> CxOptions {
        self.options
    }

    /// A token for reading and writing cells and binding handles. While it
    /// (or any handle bound to it) is alive, `self` can't be used mutably,
    /// so nothing can collect.
    #[inline]
    pub fn nogc(&self) -> NoGc<'_> {
        NoGc { cx: self }
    }

    /// Allocates a zeroed cell of at least `size` bytes. May collect
    /// first: always in stress mode, otherwise when enough was allocated
    /// since the last collection, and once more if the heap is full.
    pub fn alloc<T>(&mut self, size: u32) -> Result<Unbound<T>, AllocError> {
        let mut collected = false;
        if self.options.gc_stress || self.heap.wants_collection() {
            self.collect();
            collected = true;
        }
        let offset = match self.heap.alloc(size) {
            Err(AllocError::OutOfMemory { .. }) if !collected => {
                self.collect();
                self.heap.alloc(size)?
            }
            result => result?,
        };
        Ok(Unbound::new(offset))
    }

    /// Runs a full collection now. Everything not reachable from a scoped
    /// root or a global is freed.
    pub fn collect(&mut self) -> CollectStats {
        let handles = self.handles.borrow();
        let globals = self.globals.borrow();
        let roots = handles.iter().chain(globals.cells.iter()).copied();
        let stats = self.heap.collect(roots, &*self.tracer);
        self.collections += 1;
        stats
    }

    /// Collections run so far.
    pub fn collections(&self) -> u64 {
        self.collections
    }

    /// Bytes in allocated cells, including garbage not yet collected.
    pub fn allocated_bytes(&self) -> u64 {
        self.heap.allocated_bytes()
    }

    /// Entries on the handle stack (scoped roots of all open scopes).
    pub fn handle_count(&self) -> usize {
        self.handles.borrow().len()
    }

    /// Live [`Global`]s.
    pub fn global_count(&self) -> usize {
        let g = self.globals.borrow();
        g.cells.len() - g.free.len()
    }

    /// Runs `f` in a new root scope. Roots made with [`Scope::root`] live
    /// until `f` returns (or unwinds); they can't escape it, because `'s`
    /// is chosen fresh for each call. A result that refers to a cell must
    /// leave as an [`Unbound`] and be bound by the caller.
    pub fn scope<R>(&mut self, f: impl for<'s> FnOnce(&mut Scope<'s>) -> R) -> R {
        let base = self.handles.borrow().len();
        let id = self.id;
        let mut scope = Scope {
            cx: self,
            base,
            id,
            _brand: PhantomData,
        };
        f(&mut scope)
    }
}

/// An open root scope. Derefs to the [`Cx`]; opening a nested scope
/// borrows this one mutably, so roots can only be added to the innermost.
pub struct Scope<'s> {
    cx: &'s mut Cx,
    /// Handle stack length when the scope opened.
    base: usize,
    /// The id of the context the scope opened on. `DerefMut` lets safe
    /// code swap another context in; rooting and closing check this.
    id: u64,
    _brand: Brand<'s>,
}

impl<'s> Scope<'s> {
    /// Roots `handle` until this scope ends.
    pub fn root<T>(&self, handle: Gc<'_, T>) -> Root<'s, T> {
        assert_eq!(
            self.cx.id, self.id,
            "scope used after its context was swapped out"
        );
        let mut handles = self.cx.handles.borrow_mut();
        let index = u32::try_from(handles.len()).expect("handle stack overflow");
        handles.push(handle.offset());
        Root {
            index,
            cx: self.cx.id,
            _brand: PhantomData,
            _type: PhantomData,
        }
    }
}

impl Deref for Scope<'_> {
    type Target = Cx;
    fn deref(&self) -> &Cx {
        self.cx
    }
}

impl DerefMut for Scope<'_> {
    fn deref_mut(&mut self) -> &mut Cx {
        self.cx
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        let mut handles = self.cx.handles.borrow_mut();
        if self.cx.id == self.id && handles.len() >= self.base {
            handles.truncate(self.base);
        } else if !std::thread::panicking() {
            // Truncating another context's stack would unroot its cells.
            panic!("scope closed on a different context than it opened on");
        }
    }
}

impl fmt::Debug for Scope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// Proof that nothing collects while it lives: a shared borrow of the
/// [`Cx`]. Handles bound to it carry its lifetime.
#[derive(Clone, Copy)]
pub struct NoGc<'a> {
    cx: &'a Cx,
}

impl fmt::Debug for NoGc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoGc").field("cx", &self.cx.id).finish()
    }
}

impl<'a> NoGc<'a> {
    /// The cage, for reading raw cell memory (it can't be written through
    /// a shared reference from outside this crate).
    pub fn cage(self) -> &'a Cage {
        self.cx.heap.cage()
    }

    /// Whether `cell` is a live cell of this context's heap.
    /// The live cell that starts at `offset`, as an unrooted handle: for
    /// references the engine holds as raw offsets outside cells, such as
    /// heap references in interpreter registers (ARCHITECTURE.md §2,
    /// D-037). `None` unless `offset` is the start of a live cell (null,
    /// misaligned, out of range, inside a cell or freed all give `None`).
    /// The check always runs, verifier or not. The caller vouches for the
    /// cell type `T`, as with [`Gc::cast`].
    ///
    /// The handle obeys the usual rules: it borrows this token, so it can't
    /// be held across anything that may collect. Keeping a raw offset
    /// across a collection is the holder's job (the interpreter's frame
    /// stack is traced for that reason); this check catches the offsets of
    /// freed cells, not ones that were reused.
    pub fn handle<T>(self, offset: u32) -> Option<Gc<'a, T>> {
        if !self.cx.heap.is_live_cell(offset) {
            return None;
        }
        NonZeroU32::new(offset).map(Gc::new)
    }

    pub fn is_live<T>(self, cell: Gc<'_, T>) -> bool {
        self.cx.heap.is_live_cell(cell.offset())
    }

    /// The slot size of `cell`. Panics if it isn't live.
    pub fn cell_size<T>(self, cell: Gc<'_, T>) -> u32 {
        self.cx
            .heap
            .cell_size(cell.offset())
            .unwrap_or_else(|| panic!("{:#x} is not a live cell", cell.offset()))
    }

    /// The cage offset of `len` bytes at `field` in `cell`. With the heap
    /// verifier on, also checks that they lie inside the live cell (writes
    /// are always checked by the heap; reads otherwise only by the cage).
    #[inline]
    fn field<T>(self, cell: Gc<'_, T>, field: u32, len: u32) -> u32 {
        if self.cx.heap.options().verify {
            let size = self.cell_size(cell);
            assert!(
                field as u64 + len as u64 <= size as u64,
                "field {field:#x} (+{len}) is outside cell {:#x} of {size} bytes",
                cell.offset()
            );
        }
        cell.offset()
            .checked_add(field)
            .expect("field offset overflows the cage")
    }

    /// Reads a byte of `cell`.
    pub fn read_u8<T>(self, cell: Gc<'_, T>, field: u32) -> u8 {
        self.cage().read_u8(self.field(cell, field, 1))
    }

    /// Reads a `u32` of `cell` (4-byte aligned).
    pub fn read_u32<T>(self, cell: Gc<'_, T>, field: u32) -> u32 {
        self.cage().read_u32(self.field(cell, field, 4))
    }

    /// Reads a `u64` of `cell` (8-byte aligned).
    pub fn read_u64<T>(self, cell: Gc<'_, T>, field: u32) -> u64 {
        self.cage().read_u64(self.field(cell, field, 8))
    }

    /// Copies bytes of `cell` into `dst`.
    pub fn read_bytes<T>(self, cell: Gc<'_, T>, field: u32, dst: &mut [u8]) {
        let len = u32::try_from(dst.len()).expect("read larger than the cage");
        self.cage().read_bytes(self.field(cell, field, len), dst)
    }

    /// Reads a reference field (a `u32` offset, 0 for none) of `cell`. With
    /// the heap verifier on, panics if it points at anything but a live
    /// cell.
    pub fn read_ref<T, U>(self, cell: Gc<'_, T>, field: u32) -> Option<Gc<'a, U>> {
        let target = NonZeroU32::new(self.read_u32(cell, field))?;
        if self.cx.heap.options().verify {
            assert!(
                self.cx.heap.is_live_cell(target.get()),
                "cell {:#x} field {field:#x} refers to {target:#x}, which is not a live cell",
                cell.offset()
            );
        }
        Some(Gc::new(target))
    }

    /// Writes a byte of non-reference data into `cell`.
    pub fn write_u8<T>(self, cell: Gc<'_, T>, field: u32, value: u8) {
        self.cx.heap.write_u8(self.field(cell, field, 1), value)
    }

    /// Writes a `u32` of non-reference data into `cell`. References must
    /// go through [`NoGc::write_ref`].
    pub fn write_u32<T>(self, cell: Gc<'_, T>, field: u32, value: u32) {
        self.cx.heap.write_u32(self.field(cell, field, 4), value)
    }

    /// Writes a `u64` of non-reference data into `cell`. Values that may
    /// hold a reference must go through [`NoGc::write_value_bits`].
    pub fn write_u64<T>(self, cell: Gc<'_, T>, field: u32, value: u64) {
        self.cx.heap.write_u64(self.field(cell, field, 8), value)
    }

    /// Copies non-reference bytes into `cell`.
    pub fn write_bytes<T>(self, cell: Gc<'_, T>, field: u32, src: &[u8]) {
        let len = u32::try_from(src.len()).expect("write larger than the cage");
        self.cx.heap.write_bytes(self.field(cell, field, len), src)
    }

    /// Stores a reference to `target` (or null) in the `u32` field at
    /// `field` of `cell`, through the write barrier.
    pub fn write_ref<T, U>(self, cell: Gc<'_, T>, field: u32, target: Option<Gc<'_, U>>) {
        let target = target.map_or(0, Gc::offset);
        self.write_barrier(cell.offset(), target);
        self.cx.heap.write_u32(self.field(cell, field, 4), target)
    }

    /// Stores the 64-bit encoding of a value in `cell`, through the write
    /// barrier. `referent` is the cell the value refers to, if any (the
    /// core doesn't know the value encoding).
    pub fn write_value_bits<T, U>(
        self,
        cell: Gc<'_, T>,
        field: u32,
        bits: u64,
        referent: Option<Gc<'_, U>>,
    ) {
        self.write_barrier(cell.offset(), referent.map_or(0, Gc::offset));
        self.cx.heap.write_u64(self.field(cell, field, 8), bits)
    }

    /// The write barrier: runs before every reference store into a cell.
    /// Empty for the Phase 1 collector; generational and incremental
    /// collection fill it in (ARCHITECTURE §6.4).
    #[inline(always)]
    fn write_barrier(self, _cell: u32, _target: u32) {}

    /// Makes a [`Global`] root for `handle`.
    pub fn global<T>(self, handle: Gc<'_, T>) -> Global<T> {
        let mut g = self.cx.globals.borrow_mut();
        let slot = match g.free.pop() {
            Some(slot) => {
                g.cells[slot as usize] = handle.offset();
                slot
            }
            None => {
                let slot = u32::try_from(g.cells.len()).expect("global table overflow");
                g.cells.push(handle.offset());
                slot
            }
        };
        Global {
            slot,
            table: Rc::clone(&self.cx.globals),
            _marker: PhantomData,
        }
    }
}

/// An unrooted handle to a cell whose type marker is `T`: a 32-bit cage
/// offset, valid while the [`NoGc<'a>`] it was bound to is.
pub struct Gc<'a, T> {
    offset: NonZeroU32,
    _lifetime: PhantomData<&'a ()>,
    _type: TypeMarker<T>,
}

impl<'a, T> Gc<'a, T> {
    fn new(offset: NonZeroU32) -> Gc<'a, T> {
        Gc {
            offset,
            _lifetime: PhantomData,
            _type: PhantomData,
        }
    }

    /// The cell's cage offset.
    #[inline]
    pub fn offset(self) -> u32 {
        self.offset.get()
    }

    /// Detaches the handle from its token, to pass it to a function that
    /// may collect (which must root it before collecting) or return it from
    /// a scope. Bind the result again before any collection.
    pub fn unbind(self) -> Unbound<T> {
        Unbound::new(self.offset.get())
    }

    /// The same cell under another type marker. Cell layouts are checked by
    /// the layer that defines them, not here.
    pub fn cast<U>(self) -> Gc<'a, U> {
        Gc::new(self.offset)
    }
}

impl<T> Clone for Gc<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Gc<'_, T> {}

impl<T> PartialEq for Gc<'_, T> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
    }
}

impl<T> Eq for Gc<'_, T> {}

impl<T> Hash for Gc<'_, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.offset.hash(state)
    }
}

impl<T> fmt::Debug for Gc<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Gc({:#x})", self.offset)
    }
}

/// A cell reference with no lifetime, returned by functions that may
/// collect. Its only use is [`Unbound::bind`]; bind it right away.
#[must_use = "bind it with `.bind(cx.nogc())`; an unbound handle kept across a collection dangles"]
pub struct Unbound<T> {
    offset: NonZeroU32,
    _marker: TypeMarker<T>,
}

impl<T> Unbound<T> {
    fn new(offset: u32) -> Unbound<T> {
        Unbound {
            offset: NonZeroU32::new(offset).expect("cell offsets are never 0"),
            _marker: PhantomData,
        }
    }

    /// Gives the reference the token's lifetime. With the heap verifier or
    /// stress mode on, panics if the cell is no longer live (an `Unbound`
    /// was kept across a collection).
    pub fn bind(self, nogc: NoGc<'_>) -> Gc<'_, T> {
        if nogc.cx.options.gc_stress || nogc.cx.heap.options().verify {
            assert!(
                nogc.cx.heap.is_live_cell(self.offset.get()),
                "bound a stale handle: {:#x} is not a live cell",
                self.offset
            );
        }
        Gc::new(self.offset)
    }
}

impl<T> fmt::Debug for Unbound<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Unbound({:#x})", self.offset)
    }
}

/// A handle-stack root, valid for its scope `'s`. `Copy`: copies name the
/// same slot.
pub struct Root<'s, T> {
    index: u32,
    cx: u64,
    _brand: Brand<'s>,
    _type: TypeMarker<T>,
}

impl<'s, T> Root<'s, T> {
    fn slot<'n>(self, nogc: NoGc<'n>) -> std::cell::RefMut<'n, u32> {
        assert_eq!(self.cx, nogc.cx.id, "root used with another context");
        std::cell::RefMut::map(nogc.cx.handles.borrow_mut(), |h| {
            h.get_mut(self.index as usize)
                .expect("root outlived its scope")
        })
    }

    /// The rooted cell, as an unrooted handle (Phase 5: wherever the
    /// collector moved it).
    pub fn get<'a>(self, nogc: NoGc<'a>) -> Gc<'a, T> {
        let offset = *self.slot(nogc);
        Gc::new(NonZeroU32::new(offset).expect("roots are never null"))
    }

    /// Makes the root point at `handle` instead.
    pub fn set(self, nogc: NoGc<'_>, handle: Gc<'_, T>) {
        *self.slot(nogc) = handle.offset();
    }
}

impl<T> Clone for Root<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Root<'_, T> {}

impl<T> fmt::Debug for Root<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Root(#{})", self.index)
    }
}

/// A root in the global table, for embedders and engine caches. Lives
/// until dropped, independent of scopes.
pub struct Global<T> {
    slot: u32,
    table: Rc<RefCell<GlobalSlots>>,
    _marker: TypeMarker<T>,
}

impl<T> Global<T> {
    fn check(&self, nogc: NoGc<'_>) {
        assert!(
            Rc::ptr_eq(&self.table, &nogc.cx.globals),
            "global used with another context"
        );
    }

    /// The rooted cell, as an unrooted handle.
    pub fn get<'a>(&self, nogc: NoGc<'a>) -> Gc<'a, T> {
        self.check(nogc);
        let offset = self.table.borrow().cells[self.slot as usize];
        Gc::new(NonZeroU32::new(offset).expect("live globals are never null"))
    }

    /// Makes the global point at `handle` instead.
    pub fn set(&self, nogc: NoGc<'_>, handle: Gc<'_, T>) {
        self.check(nogc);
        self.table.borrow_mut().cells[self.slot as usize] = handle.offset();
    }
}

impl<T> Drop for Global<T> {
    fn drop(&mut self) {
        let mut g = self.table.borrow_mut();
        g.cells[self.slot as usize] = 0;
        g.free.push(self.slot);
    }
}

impl<T> fmt::Debug for Global<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Global(#{})", self.slot)
    }
}

/// Compile-fail tests for the rooting rules. Each `compile_fail` block of
/// rules 1–6 has a passing twin next to it, so a block can't pass by failing for an
/// unrelated reason (rustdoc only checks error codes on nightly).
///
/// Shared setup, hidden in each block:
///
/// ```
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: true });
/// let a = cx.alloc::<()>(8).unwrap().bind(cx.nogc());
/// cx.nogc().write_u64(a, 0, 1);
/// ```
///
/// 1. An unrooted handle can't be held across an allocation:
///
/// ```compile_fail
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: true });
/// let a = cx.alloc::<()>(8).unwrap().bind(cx.nogc());
/// let _b = cx.alloc::<()>(8).unwrap();
/// cx.nogc().write_u64(a, 0, 1);
/// ```
///
/// Rooted, it can:
///
/// ```
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: true });
/// cx.scope(|s| {
///     let a = s.alloc::<()>(8).unwrap().bind(s.nogc());
///     let a = s.root(a);
///     let _b = s.alloc::<()>(8).unwrap();
///     s.nogc().write_u64(a.get(s.nogc()), 0, 1);
/// });
/// ```
///
/// 2. Nor across an explicit collection:
///
/// ```compile_fail
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let a = cx.alloc::<()>(8).unwrap().bind(cx.nogc());
/// cx.collect();
/// let _ = cx.nogc().read_u64(a, 0);
/// ```
///
/// ```
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let a = cx.alloc::<()>(8).unwrap().bind(cx.nogc());
/// let g = cx.nogc().global(a);
/// cx.collect();
/// let _ = cx.nogc().read_u64(g.get(cx.nogc()), 0);
/// ```
///
/// 3. A root can't escape its scope:
///
/// ```compile_fail
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let r = cx.scope(|s| {
///     let a = s.alloc::<()>(8).unwrap().bind(s.nogc());
///     s.root(a)
/// });
/// ```
///
/// Not even through a captured variable:
///
/// ```compile_fail
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let mut leak = None;
/// cx.scope(|s| {
///     let a = s.alloc::<()>(8).unwrap().bind(s.nogc());
///     leak = Some(s.root(a));
/// });
/// ```
///
/// What a scope returns is an `Unbound`, bound by the caller:
///
/// ```
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let mut kept = None;
/// let u = cx.scope(|s| {
///     let a = s.alloc::<()>(8).unwrap().bind(s.nogc());
///     kept = Some(s.root(a).get(s.nogc()).offset());
///     a.unbind()
/// });
/// assert_eq!(Some(u.bind(cx.nogc()).offset()), kept);
/// ```
///
/// 4. An unrooted handle can't leave the scope either:
///
/// ```compile_fail
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let a = cx.scope(|s| s.alloc::<()>(8).unwrap().bind(s.nogc()));
/// ```
///
/// 5. Only the innermost scope can root; an outer scope is borrowed while
///    an inner one is open, so its roots can't land above the inner base:
///
/// ```compile_fail
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// cx.scope(|outer| {
///     let a = outer.alloc::<()>(8).unwrap().bind(outer.nogc()).unbind();
///     outer.scope(|inner| {
///         let _r = outer.root(a.bind(inner.nogc()));
///     });
/// });
/// ```
///
/// ```
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// cx.scope(|outer| {
///     let a = outer.alloc::<()>(8).unwrap().bind(outer.nogc());
///     let a = outer.root(a);
///     outer.scope(|inner| {
///         let _r = inner.root(a.get(inner.nogc()));
///     });
///     let _ = a.get(outer.nogc());
/// });
/// ```
///
/// 6. An `Unbound` must be used:
///
/// ```compile_fail
/// #![deny(unused_must_use)]
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// cx.alloc::<()>(8).unwrap();
/// ```
///
/// ```
/// #![deny(unused_must_use)]
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let _ = cx.alloc::<()>(8).unwrap();
/// ```
///
/// 7. A handle made from a raw offset with [`NoGc::handle`] can't be held
///    across a collection either:
///
/// ```compile_fail
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let raw = cx.alloc::<()>(8).unwrap().bind(cx.nogc()).offset();
/// let h = cx.nogc().handle::<()>(raw).unwrap();
/// cx.collect();
/// let _ = cx.nogc().read_u64(h, 0);
/// ```
///
/// ```
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// # let mut cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// let a = cx.alloc::<()>(8).unwrap().bind(cx.nogc());
/// let g = cx.nogc().global(a);
/// let raw = g.get(cx.nogc()).offset();
/// cx.collect();
/// let h = cx.nogc().handle::<()>(raw).unwrap();
/// let _ = cx.nogc().read_u64(h, 0);
/// ```
///
/// 8. The context (and so every `NoGc` token, without which a handle can't
///    be used) stays on one thread:
///
/// ```compile_fail
/// fn send<T: Send>(_: T) {}
/// # use ferrojs_core::gc::*;
/// # let heap = ferrojs_core::Heap::new(16 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// let cx = Cx::new(heap, Box::new(NoEdges), CxOptions { gc_stress: false });
/// send(cx);
/// ```
///
/// ```compile_fail
/// fn sync<T: Sync>(_: &T) {}
/// let cage = ferrojs_core::Cage::new(4 * ferrojs_core::cage::CHUNK_SIZE as u64).unwrap();
/// sync(&cage);
/// ```
#[cfg(doctest)]
pub struct RootingRules;

#[cfg(test)]
mod tests;
