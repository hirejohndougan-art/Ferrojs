//! The interpreter (task 1.7b, D-047): runs verified bytecode on the frame
//! stack of ARCHITECTURE.md §3, with closures and environments as heap
//! cells.
//!
//! - **Frame stack.** One array of 64-bit slots per [`Vm`], outside the
//!   cage ([`FrameStack`]). Frames link by slot index. Slot 0 holds the
//!   pending exception; frames start at slot 1. The heap reaches the stack
//!   through a small anchor cell held in a [`Global`]: tracing the anchor
//!   reports every reference in the live part of the stack (`0..top`).
//!   Raw header slots hold integers below 2^48, which decode as doubles,
//!   so they never look like references.
//! - **Functions** are `Function` cells: a template index (into the
//!   [`Vm`]'s Rust-side template table, D-009) and the captured
//!   environment. They become ordinary objects with the built-ins (1.13).
//! - **Environments** are `Env` cells: parent and slots. The current one
//!   is the frame's environment slot, so it is rooted with the frame.
//! - Every value the interpreter holds across an allocation lives in a
//!   stack slot or is rooted in a short nested scope; nothing else needs
//!   rooting.
//! - **Strings** (task 1.7c, D-050): constants are atoms; concatenation and
//!   ToString make flat, non-interned string cells, so strings compare by
//!   contents (code units), not by reference.
//! - **Globals** (task 1.7c, D-050): the global object is an ordinary
//!   object (prototype `Object.prototype` since 1.10a); `var` and
//!   function declarations are its non-configurable properties, implicit
//!   sloppy globals its configurable ones. The global lexical environment
//!   (`let`, `const`) is an `Env` cell, grown by copying, with the names
//!   and `const` flags in a Rust table ([`Globals`]).
//!
//! - **Properties** (task 1.7e, D-053): objects made by literals are
//!   ordinary objects with prototype `Object.prototype` (empty until the
//!   built-ins, 1.13; reading one of its future names fails cleanly). Keys go through ToPropertyKey: array indices to
//!   the elements store, other keys to named properties as atoms (a key
//!   computed at run time is interned when it is written; reads of a key
//!   no atom exists for find nothing without allocating). Strings have
//!   `length` and their indices; other primitive bases throw until their
//!   prototypes exist.
//!
//! - **Functions** (task 1.10a, D-057): closures and host functions are
//!   ordinary objects with a closure or host flag and an internal word
//!   after the inline slots (template and environment, or host function
//!   index); prototype `Function.prototype`; `length`, `name` and, for
//!   constructors, `prototype`.
//!
//! - **Host functions** (task 1.9, D-054): function objects holding an index into
//!   the [`Vm`]'s table of Rust closures ([`crate::native`]). Calling one
//!   copies `this` and the arguments out of the heap, runs the closure
//!   and stores its result; no frame is pushed.
//!
//! Error objects come with 1.13: until then a thrown error is `undefined`
//! with its [`ErrorKind`] recorded, as in the toy context.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use ferrojs_bytecode::number::{self, CompareOp, NumericOp, UnaryOp};
use ferrojs_bytecode::{
    accessor_dst, decode, step, verify, verify_handlers, Access, CallSite, Completion, Const,
    Count, Detour, EnvSlot, ErrorKind, Feedback, Flow, Func, GlobalKind, Handler, Ic, Limits,
    OpCtx, Reg, Thrown, VerifyError,
};
use ferrojs_compiler::{CompiledFunction, Constant};
use ferrojs_core::gc::{Global, Scope};
use ferrojs_core::{Gc, NoGc};
use ferrojs_value::{HeapRef, Kind, Value};

use crate::native::{DefineError, HostValue, NativeFn};
use crate::shape::Attrs;
use crate::{
    elements, object, string, value_ref, CellKind, Descriptor, JsString, Object, PropertyValue,
    RootedValue, SetResult, Tables, Vm,
};

/// Frame stack slots by default: 8 MB (ARCHITECTURE.md §3.1).
pub const DEFAULT_STACK_SLOTS: u32 = 1 << 20;
/// Header slots per frame.
pub const HEADER: u32 = 8;
/// Slots kept free above the deepest frame.
const RESERVE: u32 = 64;
/// The largest environment one `PushEnv` may make (verified bytecode can
/// ask for 2^32 slots).
pub const MAX_ENV_SLOTS: u32 = 1 << 16;
/// `caller fp` of the entry frame.
const NO_CALLER: u64 = u32::MAX as u64;
/// The longest string concatenation or ToString may make, in UTF-16 code
/// units; longer is a throw (a `RangeError` once error objects exist).
pub const MAX_STRING_UNITS: usize = 1 << 28;
/// Slots of the first global lexical environment cell.
const FIRST_LEXICAL_SLOTS: u32 = 8;

// Frame header slots (fp + n).
const CALLEE: u32 = 0;
const THIS: u32 = 1;
const NEW_TARGET: u32 = 2;
const ENV: u32 = 3;
const CALLER_FP: u32 = 4;
/// The caller's pc of the call instruction (the resume point is the
/// instruction after it; unwinding looks up handlers at the call).
const CALL_PC: u32 = 5;
/// The caller's register for the result, or [`DISCARD`].
const RETURN_INFO: u32 = 6;
const ARGC: u32 = 7;
/// `RETURN_INFO` of a call whose result is dropped (a setter's, task
/// 1.8e). Above every register number; as value bits it is a number, so
/// the tracer ignores it like the other header words.
const DISCARD: u64 = 1 << 32;

// Functions are objects (D-057): see `object::CLOSURE` and `object::HOST`.
/// Inline slots of a function object: `length`, `name`, `prototype`.
const FUNCTION_INLINE: u32 = 3;
// Env cell: header (kind | slot count << 32), parent reference, slots.
/// Offset of an environment's parent reference.
pub const ENV_PARENT: u32 = 8;
/// Offset of an environment's first slot.
pub const ENV_SLOTS: u32 = 16;

/// Cell type markers.
pub struct EnvCell;
pub struct StackAnchor;

/// The frame stack: slots outside the cage, shared with the tracer.
pub struct FrameStack {
    slots: Box<[Cell<u64>]>,
    /// Slots in use; everything below is scanned by the collector.
    top: Cell<u32>,
}

impl Default for FrameStack {
    fn default() -> FrameStack {
        FrameStack::new(0)
    }
}

impl fmt::Debug for FrameStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FrameStack({} of {})", self.top.get(), self.slots.len())
    }
}

impl FrameStack {
    /// A stack of `slots` slots (at least one: slot 0 holds the pending
    /// exception).
    pub fn new(slots: u32) -> FrameStack {
        let stack = FrameStack {
            slots: vec![Cell::new(0); slots.max(1) as usize].into_boxed_slice(),
            top: Cell::new(1),
        };
        stack.put(0, Value::UNDEFINED.to_bits());
        stack
    }

    fn capacity(&self) -> u32 {
        self.slots.len() as u32
    }

    #[inline]
    fn get(&self, i: u32) -> u64 {
        self.slots[i as usize].get()
    }

    #[inline]
    fn put(&self, i: u32, bits: u64) {
        self.slots[i as usize].set(bits)
    }

    /// Reports every reference in the live slots (for the anchor cell).
    pub(crate) fn trace(&self, edges: &mut Vec<u32>) {
        for slot in &self.slots[..self.top.get() as usize] {
            if let Some(r) = value_ref(slot.get()) {
                edges.push(r);
            }
        }
    }
}

/// A loaded function: what a closure runs.
#[derive(Debug)]
pub struct Template {
    pub name: String,
    /// Shared so the dispatch loop can hold it while handlers borrow the
    /// interpreter.
    pub code: Rc<[u8]>,
    pub handlers: Vec<Handler>,
    /// Constants as values; strings are atoms (kept alive by the atom
    /// table).
    pub constants: Vec<Value>,
    pub limits: Limits,
    pub params: u32,
    /// The closures' `length` property.
    pub length: u32,
    /// Whether closures are constructors with a `prototype` object.
    pub constructor: bool,
    /// The closures' `name` property: an atom.
    name_atom: Value,
    /// Template index of each nested function (`Func` operands).
    pub functions: Vec<u32>,
    feedback: Vec<Feedback>,
}

/// Interpreter state kept in the [`Vm`].
pub(crate) struct State {
    stack: Rc<FrameStack>,
    anchor: Option<Global<StackAnchor>>,
    templates: Vec<Template>,
    /// Atoms for the `typeof` results, by [`TypeName`].
    type_names: Vec<Value>,
    globals: Option<Globals>,
    intrinsics: Option<Intrinsics>,
    /// Host functions, by the index in their function objects.
    natives: Vec<NativeFn>,
}

/// The intrinsic objects functions and objects need (D-057), and the
/// atoms of the function properties.
pub(crate) struct Intrinsics {
    /// `Object.prototype`: an ordinary object, prototype `null`.
    object_prototype: Global<Object>,
    /// `Function.prototype`: a host function that returns `undefined`,
    /// prototype `Object.prototype`.
    function_prototype: Global<Object>,
    length: Global<JsString>,
    name: Global<JsString>,
    prototype: Global<JsString>,
    constructor: Global<JsString>,
}

impl State {
    pub(crate) fn new(stack: Rc<FrameStack>) -> State {
        State {
            stack,
            anchor: None,
            templates: Vec::new(),
            type_names: Vec::new(),
            globals: None,
            intrinsics: None,
            natives: Vec::new(),
        }
    }
}

/// The global environment (ECMA-262 §9.1.1.4): the global object for
/// `var`s, functions and implicit globals, and the global lexical
/// environment for `let` and `const`.
pub(crate) struct Globals {
    object: Global<Object>,
    /// An `Env` cell (no parent) whose first `names.len()` slots are the
    /// lexical bindings; the TDZ marker until initialized.
    lexical: Global<EnvCell>,
    names: HashMap<Vec<u16>, Binding>,
}

#[derive(Clone, Copy)]
struct Binding {
    slot: u32,
    constant: bool,
}

/// Why a function couldn't be loaded.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadError {
    /// The bytecode fails the verifier.
    Verify {
        function: String,
        error: VerifyError,
    },
    /// The heap is full.
    OutOfMemory,
}

/// How a run ended.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Outcome {
    /// The value returned. A heap reference in it is only valid until the
    /// next allocation.
    Returned(Value),
    /// An uncaught exception, and the kind of error the engine threw, if
    /// it was one of its own (the value is then `undefined` until error
    /// objects exist).
    Threw(Value, Option<ErrorKind>),
    /// The interrupt flag stopped execution (D-007).
    Terminated,
    /// An allocation failed.
    OutOfMemory,
}

#[derive(Clone, Copy)]
enum TypeName {
    Undefined,
    Object,
    Boolean,
    Number,
    String,
    Function,
    Symbol,
    Bigint,
}

const TYPE_NAMES: [&str; 8] = [
    "undefined",
    "object",
    "boolean",
    "number",
    "string",
    "function",
    "symbol",
    "bigint",
];

impl Vm {
    /// Verifies `f` and the functions nested in it and adds them to the
    /// template table; returns the index of `f`'s template.
    pub fn load(&mut self, f: &CompiledFunction) -> Result<u32, LoadError> {
        let Vm {
            cx, tables, interp, ..
        } = self;
        let before = interp.templates.len();
        let loaded = cx.scope(|s| load_tree(s, tables, &mut interp.templates, f));
        if loaded.is_err() {
            // No half-loaded tree stays behind.
            interp.templates.truncate(before);
        }
        loaded
    }

    /// Loads `f` and calls it with `this` undefined and `args`
    /// (primitives only: a reference in `args` isn't rooted while the call
    /// is set up).
    pub fn run_function(
        &mut self,
        f: &CompiledFunction,
        args: &[Value],
    ) -> Result<Outcome, LoadError> {
        let t = self.load(f)?;
        Ok(self.call_template(t, Value::UNDEFINED, args))
    }

    /// Calls a closure of template `t` (no captured environment) with
    /// `this` and `args` as the entry frame. `this` and `args` must be
    /// primitives or atoms: other references aren't rooted while the call
    /// is set up (the embedding API, 1.9, takes rooted values).
    ///
    /// # Panics
    /// If `t` isn't a template index [`Vm::load`] returned.
    pub fn call_template(&mut self, t: u32, this: Value, args: &[Value]) -> Outcome {
        let Vm {
            cx,
            tables,
            interp,
            interrupt,
            ..
        } = self;
        cx.scope(|s| {
            if prepare(s, tables, interp).is_err() {
                return Outcome::OutOfMemory;
            }
            let intrinsics = interp.intrinsics.as_ref().expect("made by prepare");
            let Ok(callee) = new_function(s, tables, intrinsics, &interp.templates, t, None) else {
                return Outcome::OutOfMemory;
            };
            let stack = interp.stack.clone();
            let base_top = stack.top.get();
            let mut it = Interp {
                s,
                stack: &stack,
                templates: &mut interp.templates,
                type_names: &interp.type_names,
                globals: interp.globals.as_mut().expect("made by prepare"),
                intrinsics: interp.intrinsics.as_ref().expect("made by prepare"),
                tables,
                natives: &mut interp.natives,
                interrupt: interrupt.as_ref(),
                fp: 0,
                tmpl: t,
                last_error: None,
                terminating: false,
                oom: false,
                pending: None,
            };
            let out = it.run(callee, this, args);
            stack.top.set(base_top);
            out
        })
    }

    /// Loads script `f` (from `compile_script`) and runs it. Scripts run
    /// in order share the global object and the global lexical
    /// environment, like several `<script>` elements.
    pub fn run_script(&mut self, f: &CompiledFunction) -> Result<Outcome, LoadError> {
        let t = self.load(f)?;
        Ok(self.call_template(t, Value::UNDEFINED, &[]))
    }

    /// The value of global `name`: its lexical binding if it has one (the
    /// TDZ marker if uninitialized), else the global object's own
    /// property (`None` for an accessor property). A heap reference in it
    /// is valid until the next allocation.
    pub fn global(&self, name: &str) -> Option<Value> {
        let g = self.interp.globals.as_ref()?;
        let nogc = self.cx.nogc();
        let units: Vec<u16> = name.encode_utf16().collect();
        if let Some(b) = g.names.get(&units) {
            return Some(lexical_get(nogc, g, b.slot));
        }
        let key = self.tables.atoms.get(&units)?.get(nogc);
        match object::get_own(nogc, g.object.get(nogc), key)? {
            (PropertyValue::Data(v), _) => Some(v),
            (PropertyValue::Accessor { .. }, _) => None,
        }
    }

    /// The code units of string value `v`, if it is a live string.
    pub fn string_units(&self, v: Value) -> Option<Vec<u16>> {
        units_of(self.cx.nogc(), v)
    }

    /// `v` copied out of the heap (for an [`Outcome`]'s value, which is
    /// only valid until the next allocation).
    pub fn host_value(&self, v: Value) -> HostValue {
        host_value(self.cx.nogc(), v)
    }

    /// Installs host function `f` as global `name`: a writable,
    /// configurable, non-enumerable property of the global object, like a
    /// built-in function (ECMA-262 §18). Replaces an earlier one of the
    /// same name. A `let` or `const` of that name shadows it, as for any
    /// global property.
    pub fn define_native(&mut self, name: &str, f: NativeFn) -> Result<(), DefineError> {
        let Vm {
            cx, tables, interp, ..
        } = self;
        // Before the index is picked: `prepare` adds `Function.prototype`'s
        // host function to the table.
        cx.scope(|s| prepare(s, tables, interp))
            .map_err(|()| DefineError::OutOfMemory)?;
        // Replacing a host function reuses its slot in the table.
        let existing = {
            let nogc = cx.nogc();
            interp.globals.as_ref().and_then(|g| {
                let units: Vec<u16> = name.encode_utf16().collect();
                let key = tables.atoms.get(&units)?.get(nogc);
                match object::get_own(nogc, g.object.get(nogc), key)? {
                    (PropertyValue::Data(v), _) => native_index(nogc, v),
                    (PropertyValue::Accessor { .. }, _) => None,
                }
            })
        };
        let index = existing.unwrap_or(interp.natives.len());
        let done = cx.scope(|s| {
            let oom = |_| DefineError::OutOfMemory;
            let intrinsics = interp.intrinsics.as_ref().expect("made by prepare");
            let k = string::atom(s, tables, name).map_err(oom)?.bind(s.nogc());
            let k = s.root(k);
            let c = new_host_function(s, tables, intrinsics, index as u32, k)
                .map_err(|()| DefineError::OutOfMemory)?
                .bind(s.nogc());
            let c = s.root(c);
            let globals = interp.globals.as_ref().expect("made by prepare");
            let o = s.root(globals.object.get(s.nogc()));
            let attrs = Attrs(Attrs::WRITABLE.0 | Attrs::CONFIGURABLE.0);
            match object::define(s, o, k, RootedValue::Object(c), attrs) {
                Ok(true) => Ok(()),
                Ok(false) => Err(DefineError::Refused),
                Err(_) => Err(DefineError::OutOfMemory),
            }
        });
        if done.is_ok() {
            match existing {
                Some(i) => interp.natives[i] = f,
                None => interp.natives.push(f),
            }
        }
        done
    }

    /// Frames' worth of slots in use (for tests: back to the base after a
    /// run).
    pub fn stack_top(&self) -> u32 {
        self.interp.stack.top.get()
    }

    /// Loaded templates.
    pub fn template(&self, t: u32) -> &Template {
        &self.interp.templates[t as usize]
    }
}

fn load_tree(
    s: &mut Scope<'_>,
    tables: &mut Tables,
    templates: &mut Vec<Template>,
    f: &CompiledFunction,
) -> Result<u32, LoadError> {
    let limits = f.limits();
    verify(&f.code, &limits)
        .and_then(|()| verify_handlers(&f.code, &f.handlers, &limits))
        .map_err(|error| LoadError::Verify {
            function: f.name.clone(),
            error,
        })?;
    let mut constants = Vec::with_capacity(f.constants.len());
    for c in &f.constants {
        constants.push(match c {
            Constant::Number(d) => Value::number(*d),
            Constant::String(text) => {
                let a = string::atom(s, tables, text)
                    .map_err(|_| LoadError::OutOfMemory)?
                    .bind(s.nogc());
                Value::string(HeapRef(a.offset()))
            }
        });
    }
    let name_atom = string::atom(s, tables, &f.name)
        .map_err(|_| LoadError::OutOfMemory)?
        .bind(s.nogc());
    let name_atom = Value::string(HeapRef(name_atom.offset()));
    let index = templates.len() as u32;
    templates.push(Template {
        name: f.name.clone(),
        code: f.code.as_slice().into(),
        handlers: f.handlers.clone(),
        constants,
        limits,
        params: f.params,
        length: f.length,
        constructor: f.constructor,
        name_atom,
        functions: Vec::new(),
        feedback: vec![Feedback::NONE; f.ics as usize],
    });
    let mut functions = Vec::with_capacity(f.functions.len());
    for g in &f.functions {
        functions.push(load_tree(s, tables, templates, g)?);
    }
    templates[index as usize].functions = functions;
    Ok(index)
}

/// Makes the stack anchor, the global environment and the `typeof`
/// atoms on first use.
fn prepare(s: &mut Scope<'_>, tables: &mut Tables, st: &mut State) -> Result<(), ()> {
    if st.anchor.is_none() {
        let a = s.alloc::<StackAnchor>(8).map_err(|_| ())?.bind(s.nogc());
        s.nogc().write_u64(a, 0, CellKind::Stack as u64);
        st.anchor = Some(s.nogc().global(a));
    }
    if st.intrinsics.is_none() {
        st.intrinsics = Some(make_intrinsics(s, tables, &mut st.natives)?);
    }
    if st.globals.is_none() {
        let proto = st
            .intrinsics
            .as_ref()
            .expect("just made")
            .object_prototype
            .get(s.nogc());
        let proto = s.root(proto);
        let o = object::new_object(s, tables, Some(proto), object::DEFAULT_INLINE)
            .map_err(|_| ())?
            .bind(s.nogc());
        let object = s.nogc().global(o);
        let e = s
            .alloc::<EnvCell>(ENV_SLOTS + 8 * FIRST_LEXICAL_SLOTS)
            .map_err(|_| ())?
            .bind(s.nogc());
        let nogc = s.nogc();
        init_env(nogc, e, FIRST_LEXICAL_SLOTS, None);
        st.globals = Some(Globals {
            object,
            lexical: nogc.global(e),
            names: HashMap::new(),
        });
    }
    if st.type_names.is_empty() {
        for name in TYPE_NAMES {
            let a = string::atom(s, tables, name)
                .map_err(|_| ())?
                .bind(s.nogc());
            st.type_names.push(Value::string(HeapRef(a.offset())));
        }
    }
    Ok(())
}

/// The intrinsics and the function property atoms (on first use).
fn make_intrinsics(
    s: &mut Scope<'_>,
    tables: &mut Tables,
    natives: &mut Vec<NativeFn>,
) -> Result<Intrinsics, ()> {
    let oom = |_| ();
    let mut atom = |s: &mut Scope<'_>, text: &str| -> Result<Global<JsString>, ()> {
        let a = string::atom(s, tables, text).map_err(oom)?.bind(s.nogc());
        Ok(s.nogc().global(a))
    };
    let length = atom(s, "length")?;
    let name = atom(s, "name")?;
    let prototype = atom(s, "prototype")?;
    let constructor = atom(s, "constructor")?;
    let empty = atom(s, "")?;
    let op = object::new_object(s, tables, None, object::DEFAULT_INLINE)
        .map_err(oom)?
        .bind(s.nogc());
    let object_prototype = s.nogc().global(op);
    // Function.prototype accepts any arguments and returns undefined
    // (ECMA-262 §20.2.3).
    let index = natives.len() as u32;
    let fp = s.scope(|n| -> Result<Global<Object>, ()> {
        let proto = n.root(object_prototype.get(n.nogc()));
        let f = object::new_function_object(n, tables, proto, FUNCTION_INLINE, object::HOST)
            .map_err(oom)?
            .bind(n.nogc());
        let nogc = n.nogc();
        nogc.write_u32(f, object::internal_word(nogc, f), index);
        let f = n.root(f);
        let (l, nm) = (n.root(length.get(n.nogc())), n.root(name.get(n.nogc())));
        let e = RootedValue::String(n.root(empty.get(n.nogc())));
        define_fixed(n, f, l, RootedValue::Primitive(Value::from_i32(0)))?;
        define_fixed(n, f, nm, e)?;
        Ok(n.nogc().global(f.get(n.nogc())))
    })?;
    // Only once it exists: a failed attempt leaves no dead entry.
    natives.push(Box::new(|_, _| Ok(HostValue::Undefined)));
    Ok(Intrinsics {
        object_prototype,
        function_prototype: fp,
        length,
        name,
        prototype,
        constructor,
    })
}

/// Defines `length` or `name` on a new function: not writable, not
/// enumerable, configurable (ECMA-262 §10.2.9, §10.2.10).
fn define_fixed(
    s: &mut Scope<'_>,
    f: ferrojs_core::gc::Root<'_, Object>,
    key: ferrojs_core::gc::Root<'_, JsString>,
    v: RootedValue<'_>,
) -> Result<(), ()> {
    match object::define(s, f, key, v, Attrs::CONFIGURABLE) {
        Ok(true) => Ok(()),
        _ => Err(()),
    }
}

/// A new closure of template `t` (OrdinaryFunctionCreate, plus
/// MakeConstructor for constructors); `env` is an environment cell offset
/// (rooted by the caller: it is read again after the allocation).
fn new_function(
    s: &mut Scope<'_>,
    tables: &mut Tables,
    intr: &Intrinsics,
    templates: &[Template],
    t: u32,
    env: Option<&dyn Fn() -> u32>,
) -> Result<Value, ()> {
    let tmpl = &templates[t as usize];
    let (length, name_atom, constructor) = (tmpl.length, tmpl.name_atom, tmpl.constructor);
    s.scope(|n| {
        let oom = |_| ();
        let proto = n.root(intr.function_prototype.get(n.nogc()));
        let f = object::new_function_object(n, tables, proto, FUNCTION_INLINE, object::CLOSURE)
            .map_err(oom)?
            .bind(n.nogc());
        let nogc = n.nogc();
        let at = object::internal_word(nogc, f);
        nogc.write_u32(f, at, t);
        let env = env.map_or(0, |e| e());
        nogc.write_ref(f, at + 4, nogc.handle::<EnvCell>(env));
        let f = n.root(f);
        let len = n.root(intr.length.get(n.nogc()));
        let length = Value::from_i32(i32::try_from(length).unwrap_or(i32::MAX));
        define_fixed(n, f, len, RootedValue::Primitive(length))?;
        let key = n.root(intr.name.get(n.nogc()));
        let name = rooted(n, name_atom).ok_or(())?;
        define_fixed(n, f, key, name)?;
        if constructor {
            // MakeConstructor: a fresh prototype object whose
            // `constructor` is the function.
            let op = n.root(intr.object_prototype.get(n.nogc()));
            let p = object::new_object(n, tables, Some(op), object::DEFAULT_INLINE)
                .map_err(oom)?
                .bind(n.nogc());
            let p = n.root(p);
            let key = n.root(intr.constructor.get(n.nogc()));
            let attrs = Attrs(Attrs::WRITABLE.0 | Attrs::CONFIGURABLE.0);
            if object::define(n, p, key, RootedValue::Object(f), attrs) != Ok(true) {
                return Err(());
            }
            let key = n.root(intr.prototype.get(n.nogc()));
            if object::define(n, f, key, RootedValue::Object(p), Attrs::WRITABLE) != Ok(true) {
                return Err(());
            }
        }
        Ok(Value::object(HeapRef(f.get(n.nogc()).offset())))
    })
}

/// A new host function object for table entry `index`, named `name`
/// (`length` 0; not a constructor).
fn new_host_function<'s>(
    s: &mut Scope<'s>,
    tables: &mut Tables,
    intr: &Intrinsics,
    index: u32,
    name: ferrojs_core::gc::Root<'_, JsString>,
) -> Result<ferrojs_core::Unbound<Object>, ()> {
    let oom = |_| ();
    let proto = s.root(intr.function_prototype.get(s.nogc()));
    let f = object::new_function_object(s, tables, proto, FUNCTION_INLINE, object::HOST)
        .map_err(oom)?
        .bind(s.nogc());
    let nogc = s.nogc();
    nogc.write_u32(f, object::internal_word(nogc, f), index);
    let f = s.root(f);
    let len = s.root(intr.length.get(s.nogc()));
    define_fixed(s, f, len, RootedValue::Primitive(Value::from_i32(0)))?;
    let key = s.root(intr.name.get(s.nogc()));
    define_fixed(s, f, key, RootedValue::String(name))?;
    Ok(f.get(s.nogc()).unbind())
}

/// Function object `v`'s header flags and internal word, if `v` is a
/// live function object.
/// On the call path: one header read, one internal word read.
#[inline]
fn function_word(nogc: NoGc<'_>, v: Value) -> Option<(u64, u64)> {
    let Kind::Object(HeapRef(r)) = v.kind() else {
        return None;
    };
    let o = nogc.handle::<Object>(r)?;
    let header = nogc.read_u64(o, 0);
    if header as u8 != CellKind::Object as u8 || header & (object::CLOSURE | object::HOST) == 0 {
        return None;
    }
    let word = object::INLINE + 8 * object::inline_count(header);
    Some((header & 0xff00, nogc.read_u64(o, word)))
}

/// Fills a new environment cell of `count` slots with `undefined`.
fn init_env(nogc: NoGc<'_>, e: Gc<'_, EnvCell>, count: u32, parent: Option<Gc<'_, EnvCell>>) {
    nogc.write_u64(e, 0, CellKind::Env as u64 | (u64::from(count) << 32));
    nogc.write_ref(e, ENV_PARENT, parent);
    for i in 0..count {
        nogc.write_u64(e, ENV_SLOTS + 8 * i, Value::UNDEFINED.to_bits());
    }
}

/// Global lexical binding `slot`'s value.
fn lexical_get(nogc: NoGc<'_>, g: &Globals, slot: u32) -> Value {
    let bits = nogc.read_u64(g.lexical.get(nogc), ENV_SLOTS + 8 * slot);
    Value::from_bits_checked(bits).unwrap_or(Value::UNINITIALIZED)
}

/// The code units of string value `v`, if it is a live string cell.
fn units_of(nogc: NoGc<'_>, v: Value) -> Option<Vec<u16>> {
    let Kind::String(HeapRef(r)) = v.kind() else {
        return None;
    };
    let s = nogc.handle::<JsString>(r)?;
    (nogc.read_u8(s, 0) == CellKind::String as u8).then(|| string::units(nogc, s))
}

/// `v` rooted in scope `n` (for storing it after allocations), or `None`
/// for values a property can't hold (the TDZ marker, dead references).
fn rooted<'n>(n: &Scope<'n>, v: Value) -> Option<RootedValue<'n>> {
    let nogc = n.nogc();
    match v.kind() {
        Kind::String(HeapRef(r)) => Some(RootedValue::String(n.root(nogc.handle(r)?))),
        Kind::Object(HeapRef(r)) => Some(RootedValue::Object(n.root(nogc.handle(r)?))),
        Kind::Other(..) | Kind::Uninitialized => None,
        _ => Some(RootedValue::Primitive(v)),
    }
}

/// How a call went on: in a new frame, or done (a host function's
/// result).
enum Called {
    Frame,
    Native(Value),
}

/// The host function index of `v`, if it is a live host function.
fn native_index(nogc: NoGc<'_>, v: Value) -> Option<usize> {
    match function_word(nogc, v)? {
        (flags, word) if flags & object::HOST != 0 => Some(word as u32 as usize),
        _ => None,
    }
}

/// `v` copied out of the heap ([`HostValue`]).
fn host_value(nogc: NoGc<'_>, v: Value) -> HostValue {
    match v.kind() {
        Kind::Undefined | Kind::Uninitialized => HostValue::Undefined,
        Kind::Null => HostValue::Null,
        Kind::Bool(b) => HostValue::Bool(b),
        Kind::Int32(_) | Kind::Double(_) => HostValue::Number(v.as_number().unwrap_or(f64::NAN)),
        Kind::String(_) => match units_of(nogc, v) {
            Some(u) => HostValue::String(String::from_utf16_lossy(&u)),
            None => HostValue::Undefined,
        },
        Kind::Object(_) => {
            if function_word(nogc, v).is_some() {
                HostValue::Function
            } else {
                HostValue::Object
            }
        }
        // No symbols or BigInts exist yet.
        Kind::Other(..) => HostValue::Object,
    }
}

/// The cell `v` refers to if it is a live cell of `kind`.
fn cell_of<'a, T>(nogc: NoGc<'a>, v: Value, kind: CellKind) -> Option<Gc<'a, T>> {
    let Kind::Object(HeapRef(r)) = v.kind() else {
        return None;
    };
    let c = nogc.handle::<T>(r)?;
    (nogc.read_u8(c, 0) == kind as u8).then_some(c)
}

/// Writes value `v` into `cell` at `field`, with the barrier.
fn store<T>(nogc: NoGc<'_>, cell: Gc<'_, T>, field: u32, v: Value) {
    let bits = v.to_bits();
    let referent = value_ref(bits).and_then(|r| nogc.handle::<()>(r));
    nogc.write_value_bits(cell, field, bits, referent);
}

/// A property key after ToPropertyKey.
enum Key {
    /// An array index: the elements store.
    Index(u32),
    /// Any other key, as code units: a named property.
    Name(Vec<u16>),
}

/// The base of a property access.
enum Base<'a> {
    Object(Gc<'a, Object>),
    /// A string primitive and its length.
    String(Gc<'a, JsString>, u32),
    /// A number or boolean.
    Primitive,
    Nullish,
    /// Anything that isn't a value (the TDZ marker, a dead reference).
    Other,
}

/// ToNumber for primitives other than strings and symbols.
fn to_number(v: Value) -> Option<f64> {
    match v.kind() {
        Kind::Undefined => Some(f64::NAN),
        Kind::Null => Some(0.0),
        Kind::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
        Kind::Int32(i) => Some(f64::from(i)),
        Kind::Double(d) => Some(d),
        Kind::Object(_) | Kind::String(_) | Kind::Other(..) | Kind::Uninitialized => None,
    }
}

/// The interpreter over one entry call.
struct Interp<'r, 's> {
    s: &'r mut Scope<'s>,
    stack: &'r FrameStack,
    templates: &'r mut Vec<Template>,
    type_names: &'r [Value],
    globals: &'r mut Globals,
    intrinsics: &'r Intrinsics,
    tables: &'r mut Tables,
    natives: &'r mut Vec<NativeFn>,
    interrupt: &'r std::sync::atomic::AtomicBool,
    fp: u32,
    tmpl: u32,
    last_error: Option<ErrorKind>,
    terminating: bool,
    oom: bool,
    /// The getter or setter call a property op left for the dispatch
    /// loop ([`Flow::Accessor`]). Its values are references read from
    /// live objects; the loop writes them into a frame before anything
    /// can allocate.
    pending: Option<PendingCall>,
}

/// An accessor call waiting for the dispatch loop.
#[derive(Clone, Copy, Debug)]
struct PendingCall {
    callee: Value,
    this: Value,
    /// The setter's argument; `None` for a getter.
    arg: Option<Value>,
}

impl Interp<'_, '_> {
    #[inline]
    fn slot(&self, n: u32) -> u64 {
        self.stack.get(self.fp + n)
    }

    #[inline]
    fn set_slot(&self, n: u32, bits: u64) {
        self.stack.put(self.fp + n, bits)
    }

    fn template(&self) -> &Template {
        &self.templates[self.tmpl as usize]
    }

    /// The running function's code.
    fn code(&self) -> Rc<[u8]> {
        self.template().code.clone()
    }

    /// The end of the current frame: registers, then extra arguments.
    fn frame_end(&self) -> u32 {
        let t = self.template();
        let extra = (self.slot(ARGC) as u32).saturating_sub(t.params);
        self.fp + HEADER + t.limits.registers + extra
    }

    fn env(&self) -> Value {
        Value::from_bits_checked(self.slot(ENV)).unwrap_or(Value::UNDEFINED)
    }

    /// The template of closure `callee` (a value in a frame).
    #[inline]
    fn template_of(&self, callee: Value) -> Option<u32> {
        match function_word(self.s.nogc(), callee)? {
            (flags, word) if flags & object::CLOSURE != 0 => Some(word as u32),
            _ => None,
        }
    }

    /// The host function index of `callee`, if it is a host function.
    fn native_of(&self, callee: Value) -> Option<usize> {
        native_index(self.s.nogc(), callee)
    }

    /// `v` copied out of the heap for a host function.
    fn host_value(&self, v: Value) -> HostValue {
        host_value(self.s.nogc(), v)
    }

    /// A host function's result as a value (strings are allocated);
    /// objects and functions can't be made from the host side.
    fn value_of_host(&mut self, h: HostValue) -> Completion {
        match h {
            HostValue::Undefined => Ok(Value::UNDEFINED),
            HostValue::Null => Ok(Value::NULL),
            HostValue::Bool(b) => Ok(Value::from_bool(b)),
            HostValue::Number(x) => Ok(Value::number(x)),
            HostValue::String(t) => {
                let units: Vec<u16> = t.encode_utf16().collect();
                self.make_string(&units)
            }
            HostValue::Object | HostValue::Function => Err(self.error(ErrorKind::HostResult)),
        }
    }

    /// Calls host function `i` with `this` and `args`: its result, or
    /// the throw (its error value, already pending).
    fn call_native(&mut self, i: usize, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        if let Flow::Throw = self.check_interrupt() {
            return Err(Thrown);
        }
        // Copies: nothing below holds a heap reference while the host
        // function runs, and it can't allocate on the engine's heap.
        let this = self.host_value(this);
        let args: Vec<HostValue> = args.iter().map(|&a| self.host_value(a)).collect();
        let f = self
            .natives
            .get_mut(i)
            .expect("native cells index the table");
        match f(&this, &args) {
            Ok(h) => self.value_of_host(h),
            Err(h) => {
                let v = self.value_of_host(h)?;
                self.last_error = None;
                self.stack.put(0, v.to_bits());
                Err(Thrown)
            }
        }
    }

    fn error(&mut self, e: ErrorKind) -> Thrown {
        self.last_error = Some(e);
        self.stack.put(0, Value::UNDEFINED.to_bits());
        Thrown
    }

    /// A throw for what this interpreter can't do yet (strings, objects):
    /// `undefined` with no error kind, like the toy context.
    fn fail<T>(&mut self) -> Result<T, Thrown> {
        self.last_error = None;
        self.stack.put(0, Value::UNDEFINED.to_bits());
        Err(Thrown)
    }

    /// Allocation failed: stop, uncatchably.
    fn out_of_memory(&mut self) -> Thrown {
        self.oom = true;
        self.terminating = true;
        self.stack.put(0, Value::UNDEFINED.to_bits());
        Thrown
    }

    fn numbers(&mut self, a: Value, b: Value, f: impl FnOnce(f64, f64) -> f64) -> Completion {
        match (self.num(a), self.num(b)) {
            (Some(x), Some(y)) => Ok(Value::number(f(x, y))),
            _ => self.fail(),
        }
    }

    fn predicate(&mut self, a: Value, b: Value, f: impl FnOnce(f64, f64) -> bool) -> Completion {
        match (self.num(a), self.num(b)) {
            (Some(x), Some(y)) => Ok(Value::from_bool(f(x, y))),
            _ => self.fail(),
        }
    }

    /// The code units of string value `v`.
    fn units(&self, v: Value) -> Option<Vec<u16>> {
        units_of(self.s.nogc(), v)
    }

    /// The length of string value `v`, without copying it.
    fn str_len(&self, v: Value) -> Option<u32> {
        let Kind::String(HeapRef(r)) = v.kind() else {
            return None;
        };
        let nogc = self.s.nogc();
        let s = nogc.handle::<JsString>(r)?;
        (nogc.read_u8(s, 0) == CellKind::String as u8).then(|| string::len(nogc, s))
    }

    /// Whether `a` and `b` are strings with the same contents.
    fn same_string(&self, a: Value, b: Value) -> bool {
        if a.is_string() && a == b {
            return true;
        }
        match (self.str_len(a), self.str_len(b)) {
            (Some(x), Some(y)) if x == y => {
                matches!((self.units(a), self.units(b)), (Some(x), Some(y)) if x == y)
            }
            _ => false,
        }
    }

    /// ToNumber for primitives other than symbols (strings by
    /// StringToNumber); `None` for objects until ToPrimitive exists.
    fn num(&self, v: Value) -> Option<f64> {
        if v.is_string() {
            // A lone surrogate becomes U+FFFD, which is just as invalid in
            // a StringNumericLiteral: the result is NaN either way.
            return self
                .units(v)
                .map(|u| number::string_to_number(&String::from_utf16_lossy(&u)));
        }
        to_number(v)
    }

    /// ToString for primitives other than symbols, as code units.
    fn text(&self, v: Value) -> Option<Vec<u16>> {
        let t = match v.kind() {
            Kind::String(_) => return self.units(v),
            Kind::Undefined => "undefined".to_owned(),
            Kind::Null => "null".to_owned(),
            Kind::Bool(b) => b.to_string(),
            _ => number::to_js_string(v.as_number()?),
        };
        Some(t.encode_utf16().collect())
    }

    /// Code-unit order of two strings, if both are.
    fn string_order(&self, a: Value, b: Value) -> Option<std::cmp::Ordering> {
        if !(a.is_string() && b.is_string()) {
            return None;
        }
        Some(self.units(a)?.cmp(&self.units(b)?))
    }

    /// A new (not interned) string cell holding `units`.
    fn make_string(&mut self, units: &[u16]) -> Completion {
        if units.len() > MAX_STRING_UNITS {
            return self.fail();
        }
        match string::alloc_units(self.s, units) {
            Ok(g) => Ok(Value::string(HeapRef(g.bind(self.s.nogc()).offset()))),
            Err(_) => Err(self.out_of_memory()),
        }
    }

    /// The atom offset and code units of a global name (a string
    /// constant, so an atom); a throw for anything else.
    fn name_key(&mut self, name: Value) -> Result<(u32, Vec<u16>), Thrown> {
        let nogc = self.s.nogc();
        if let Kind::String(HeapRef(r)) = name.kind() {
            if let Some(k) = nogc.handle::<JsString>(r) {
                if nogc.read_u8(k, 0) == CellKind::String as u8 && string::is_atom(nogc, k) {
                    return Ok((r, string::units(nogc, k)));
                }
            }
        }
        self.fail()
    }

    /// ToPropertyKey for primitives other than symbols; a throw for the
    /// rest (objects need ToPrimitive, 1.13).
    fn key(&mut self, k: Value) -> Result<Key, Thrown> {
        if let Some(i) = k.as_int32() {
            if let Ok(i) = u32::try_from(i) {
                return Ok(Key::Index(i));
            }
        }
        if let Some(d) = k.as_number() {
            if let Some(i) = number::array_index_of_number(d) {
                return Ok(Key::Index(i));
            }
        }
        if matches!(
            k.kind(),
            Kind::Object(_) | Kind::Other(..) | Kind::Uninitialized
        ) {
            return self.fail();
        }
        let Some(units) = self.text(k) else {
            return self.fail();
        };
        Ok(match number::array_index_of_units(&units) {
            Some(i) => Key::Index(i),
            None => Key::Name(units),
        })
    }

    /// What kind of base `v` is.
    fn base(&self, v: Value) -> Base<'_> {
        let nogc = self.s.nogc();
        match v.kind() {
            Kind::Undefined | Kind::Null => Base::Nullish,
            Kind::Bool(_) | Kind::Int32(_) | Kind::Double(_) => Base::Primitive,
            Kind::String(HeapRef(r)) => match nogc.handle::<JsString>(r) {
                Some(s) if nogc.read_u8(s, 0) == CellKind::String as u8 => {
                    Base::String(s, string::len(nogc, s))
                }
                _ => Base::Other,
            },
            Kind::Object(_) => match cell_of::<Object>(nogc, v, CellKind::Object) {
                Some(o) => Base::Object(o),
                None => Base::Other,
            },
            Kind::Other(..) | Kind::Uninitialized => Base::Other,
        }
    }

    /// The existing atom for `units`, if any (no allocation: a key no
    /// atom exists for is on no object).
    fn find_atom(&self, units: &[u16]) -> Option<Gc<'_, JsString>> {
        let nogc = self.s.nogc();
        self.tables.atoms.get(units).map(|g| g.get(nogc))
    }

    /// \[\[HasProperty\]\] on an ordinary object: own, then up the
    /// prototype chain. An array's `length` is always there.
    fn has(&self, o: Gc<'_, Object>, key: &Key) -> bool {
        let nogc = self.s.nogc();
        match key {
            Key::Index(i) => {
                let mut cur = Some(o.cast::<Object>());
                while let Some(obj) = cur {
                    if elements::get_own(nogc, obj, *i).is_some() {
                        return true;
                    }
                    cur = object::prototype(nogc, obj);
                }
                false
            }
            Key::Name(units) => {
                if is_length(units) && elements::is_array(nogc, o) {
                    return true;
                }
                let Some(k) = self.find_atom(units) else {
                    return false;
                };
                let mut cur = Some(o.cast::<Object>());
                while let Some(obj) = cur {
                    if object::get_own(nogc, obj, k).is_some() {
                        return true;
                    }
                    cur = object::prototype(nogc, obj);
                }
                false
            }
        }
    }

    /// Sets (`define` false: OrdinarySet) or defines (CreateDataProperty)
    /// property `key` of object `o` to `v`, interning a named key. Returns
    /// whether the object accepted it.
    fn put(&mut self, o: Value, key: Key, v: Value, define: bool) -> Result<SetResult, Thrown> {
        let tables = &mut *self.tables;
        let done = self.s.scope(|n| {
            let nogc = n.nogc();
            let obj = cell_of::<Object>(nogc, o, CellKind::Object).expect("checked by the caller");
            let obj = n.root(obj);
            let Some(v) = rooted(n, v) else {
                return Ok(None);
            };
            match key {
                Key::Index(i) => elements::set(n, obj, i, v).map(|ok| Some(SetResult::Done(ok))),
                Key::Name(units) => {
                    if is_length(&units) && elements::is_array(n.nogc(), obj.get(n.nogc())) {
                        // ArraySetLength comes with the Array built-ins.
                        return Ok(None);
                    }
                    let k = string::atom_units(n, tables, &units)?.bind(n.nogc());
                    let k = n.root(k);
                    if define {
                        object::define(n, obj, k, v, Attrs::DEFAULT)
                            .map(|ok| Some(SetResult::Done(ok)))
                    } else {
                        object::set(n, obj, k, v).map(Some)
                    }
                }
            }
        });
        match done {
            Ok(Some(r)) => Ok(r),
            // A value no property can hold, or an array length.
            Ok(None) => self.fail(),
            Err(_) => Err(self.out_of_memory()),
        }
    }

    /// The global lexical binding named `units`.
    fn lexical(&self, units: &[u16]) -> Option<Binding> {
        self.globals.names.get(units).copied()
    }

    fn lexical_value(&self, b: Binding) -> Value {
        lexical_get(self.s.nogc(), self.globals, b.slot)
    }

    fn lexical_set(&self, slot: u32, v: Value) {
        let nogc = self.s.nogc();
        store(
            nogc,
            self.globals.lexical.get(nogc),
            ENV_SLOTS + 8 * slot,
            v,
        );
    }

    /// Adds an uninitialized global lexical binding, growing the
    /// environment cell (by copying) when it is full.
    fn lexical_add(&mut self, units: Vec<u16>, constant: bool) -> Result<(), Thrown> {
        let Ok(slot) = u32::try_from(self.globals.names.len()) else {
            return self.fail();
        };
        let nogc = self.s.nogc();
        let cap = (nogc.read_u64(self.globals.lexical.get(nogc), 0) >> 32) as u32;
        if slot >= cap {
            let Some(new_cap) = cap.checked_mul(2).filter(|&c| {
                c.checked_mul(8)
                    .and_then(|b| b.checked_add(ENV_SLOTS))
                    .is_some()
            }) else {
                return self.fail();
            };
            // The old cell stays reachable through its global until the
            // new one replaces it.
            let e = match self.s.alloc::<EnvCell>(ENV_SLOTS + 8 * new_cap) {
                Ok(e) => e.bind(self.s.nogc()),
                Err(_) => return Err(self.out_of_memory()),
            };
            let nogc = self.s.nogc();
            init_env(nogc, e, new_cap, None);
            let old = self.globals.lexical.get(nogc);
            for i in 0..cap {
                let v = Value::from_bits_checked(nogc.read_u64(old, ENV_SLOTS + 8 * i))
                    .unwrap_or(Value::UNINITIALIZED);
                store(nogc, e, ENV_SLOTS + 8 * i, v);
            }
            self.globals.lexical.set(nogc, e);
        }
        self.lexical_set(slot, Value::UNINITIALIZED);
        self.globals.names.insert(units, Binding { slot, constant });
        Ok(())
    }

    /// The global object's own property `key` (an atom offset).
    fn global_own(&self, key: u32) -> Option<(PropertyValue, Attrs)> {
        let nogc = self.s.nogc();
        object::get_own(nogc, self.globals.object.get(nogc), nogc.handle(key)?)
    }

    /// Sets (`attrs` `None`: OrdinarySet) or defines the global object's
    /// property `key` (an atom offset) to `v`.
    fn global_put(&mut self, key: u32, v: Value, attrs: Option<Attrs>) -> Result<(), Thrown> {
        let object = &self.globals.object;
        let done = self.s.scope(|n| {
            let nogc = n.nogc();
            let o = n.root(object.get(nogc));
            let k = n.root(nogc.handle::<JsString>(key).expect("names are live atoms"));
            let Some(v) = rooted(n, v) else {
                return Ok(false);
            };
            match attrs {
                // A setter (none on the global object yet) is refused
                // until task 1.8e calls it.
                None => object::set(n, o, k, v).map(|r| r == SetResult::Done(true)),
                Some(a) => object::define(n, o, k, v, a),
            }
        });
        match done {
            Ok(true) => Ok(()),
            // Refused (nothing on the global object is non-writable yet)
            // or a value no property can hold.
            Ok(false) => self.fail(),
            Err(_) => Err(self.out_of_memory()),
        }
    }

    /// The environment `depth` levels out and slot `slot`, if in range.
    fn env_cell(&self, slot: EnvSlot) -> Option<(Gc<'_, EnvCell>, u32)> {
        let nogc = self.s.nogc();
        let mut e = cell_of::<EnvCell>(nogc, self.env(), CellKind::Env)?;
        for _ in 0..slot.depth {
            e = nogc.read_ref(e, ENV_PARENT)?;
        }
        let count = (nogc.read_u64(e, 0) >> 32) as u32;
        (slot.slot < count).then_some((e, ENV_SLOTS + 8 * slot.slot))
    }

    /// Allocates an environment of `count` slots with `parent` (read after
    /// the allocation, so it must be rooted: the frame's environment),
    /// filled by `fill`.
    fn new_env(&mut self, count: u32, fill: impl Fn(NoGc<'_>, u32) -> Value) -> Result<(), Thrown> {
        if count > MAX_ENV_SLOTS {
            return self.fail();
        }
        let e = match self.s.alloc::<EnvCell>(ENV_SLOTS + 8 * count) {
            Ok(e) => e.bind(self.s.nogc()),
            Err(_) => return Err(self.out_of_memory()),
        };
        let nogc = self.s.nogc();
        let parent = cell_of::<EnvCell>(nogc, self.env(), CellKind::Env);
        nogc.write_u64(e, 0, CellKind::Env as u64 | (u64::from(count) << 32));
        nogc.write_ref(e, ENV_PARENT, parent);
        for i in 0..count {
            store(nogc, e, ENV_SLOTS + 8 * i, fill(nogc, i));
        }
        self.set_slot(ENV, Value::object(HeapRef(e.offset())).to_bits());
        Ok(())
    }

    /// Runs `callee` as the entry frame until it returns or throws.
    fn run(&mut self, callee: Value, this: Value, args: &[Value]) -> Outcome {
        let top = self.stack.top.get();
        // The entry frame: callee, this and arguments are written before
        // anything can collect.
        match self.push_frame(top, callee, this, args, NO_CALLER, 0, 0) {
            Ok(()) => {}
            Err(Thrown) => return self.uncaught(),
        }
        let mut pc = 0usize;
        // The running function's code; replaced whenever the frame changes.
        let mut code = self.code();
        loop {
            let (flow, len) = step(self, &code, pc);
            let flow = match flow {
                Flow::Call => {
                    // Calls read their operands again (one decode per call,
                    // none per other instruction).
                    let (insn, _) = decode(&code, pc).expect("verified bytecode decodes");
                    let site = CallSite::of(&insn).expect("only calls return Flow::Call");
                    match self.call(site, pc) {
                        Ok(Called::Frame) => {
                            pc = 0;
                            code = self.code();
                            continue;
                        }
                        // A host function ran without a frame: a tail call
                        // returns its result from this frame.
                        Ok(Called::Native(v)) if site.tail => Flow::Return(v),
                        Ok(Called::Native(v)) => {
                            self.set(site.dst, v);
                            Flow::Next
                        }
                        Err(Thrown) => Flow::Throw,
                    }
                }
                Flow::Accessor => {
                    let (insn, _) = decode(&code, pc).expect("verified bytecode decodes");
                    let dst = accessor_dst(&insn);
                    match self.call_accessor(dst, pc) {
                        Ok(Called::Frame) => {
                            pc = 0;
                            code = self.code();
                            continue;
                        }
                        Ok(Called::Native(v)) => {
                            if let Some(d) = dst {
                                self.set(d, v);
                            }
                            Flow::Next
                        }
                        Err(Thrown) => Flow::Throw,
                    }
                }
                flow => flow,
            };
            match flow {
                Flow::Next => pc += len,
                Flow::Jump(l) => {
                    pc = usize::try_from(pc as i64 + i64::from(l.0)).expect("verified jump")
                }
                Flow::Return(v) => {
                    let caller = self.slot(CALLER_FP);
                    if caller == NO_CALLER {
                        return Outcome::Returned(v);
                    }
                    let call_pc = self.slot(CALL_PC) as usize;
                    let info = self.slot(RETURN_INFO);
                    self.fp = caller as u32;
                    self.tmpl = self
                        .template_of(Value::from_bits_checked(self.slot(CALLEE)).expect("callee"))
                        .expect("a caller frame runs a function");
                    self.stack.top.set(self.frame_end());
                    code = self.code();
                    let (_, call_len) = decode(&code, call_pc).expect("verified call");
                    pc = call_pc + call_len;
                    if info & DISCARD == 0 {
                        self.set(Reg(info as u32), v);
                    }
                }
                Flow::Throw => match self.unwind(pc) {
                    Some(target) => {
                        pc = target;
                        code = self.code();
                    }
                    None => return self.uncaught(),
                },
                Flow::Call | Flow::Accessor => unreachable!("handled above"),
            }
        }
    }

    fn uncaught(&mut self) -> Outcome {
        if self.oom {
            Outcome::OutOfMemory
        } else if self.terminating {
            Outcome::Terminated
        } else {
            let v = Value::from_bits_checked(self.stack.get(0)).unwrap_or(Value::UNDEFINED);
            self.stack.put(0, Value::UNDEFINED.to_bits());
            Outcome::Threw(v, self.last_error)
        }
    }

    /// Finds the handler for the pending exception, popping frames: the
    /// pc to continue at, or `None` if it leaves the entry frame.
    fn unwind(&mut self, mut pc: usize) -> Option<usize> {
        loop {
            if !self.terminating {
                let h = self
                    .template()
                    .handlers
                    .iter()
                    .find(|h| (h.start as usize..h.end as usize).contains(&pc))
                    .copied();
                if let Some(h) = h {
                    let v = Value::from_bits_checked(self.stack.get(0)).unwrap_or(Value::UNDEFINED);
                    self.stack.put(0, Value::UNDEFINED.to_bits());
                    self.set(h.exception, v);
                    return Some(h.target as usize);
                }
            }
            let caller = self.slot(CALLER_FP);
            if caller == NO_CALLER {
                return None;
            }
            pc = self.slot(CALL_PC) as usize;
            self.fp = caller as u32;
            self.tmpl = self
                .template_of(Value::from_bits_checked(self.slot(CALLEE)).expect("callee"))
                .expect("a caller frame runs a function");
            self.stack.top.set(self.frame_end());
        }
    }

    /// Performs a call from the instruction at `pc`: on success the new
    /// frame is current and runs from pc 0.
    fn call(&mut self, site: CallSite, pc: usize) -> Result<Called, Thrown> {
        let callee = self.reg(site.callee);
        let Some(t) = self.template_of(callee) else {
            // Bytecode functions first: host calls are the rare case.
            if let Some(i) = self.native_of(callee) {
                let this = self.reg(site.this);
                let args: Vec<Value> = (0..site.argc.0)
                    .map(|a| self.reg(Reg(site.args.0 + a)))
                    .collect();
                return self.call_native(i, this, &args).map(Called::Native);
            }
            return Err(self.error(ErrorKind::NotCallable));
        };
        // Stack overflow before the interrupt check, as in the toy
        // context (`push_frame` checks again with the exact frame).
        let start = if site.tail { self.fp } else { self.frame_end() };
        if !self.fits(start, t, site.argc.0) {
            return Err(self.error(ErrorKind::StackOverflow));
        }
        if let Flow::Throw = self.check_interrupt() {
            return Err(Thrown);
        }
        let this = self.reg(site.this);
        let args: Vec<Value> = (0..site.argc.0)
            .map(|i| self.reg(Reg(site.args.0 + i)))
            .collect();
        if site.tail {
            // Keep this frame's link to its caller (ARCHITECTURE.md §3.4).
            let (caller, call_pc, info) = (
                self.slot(CALLER_FP),
                self.slot(CALL_PC),
                self.slot(RETURN_INFO),
            );
            let fp = self.fp;
            self.push_frame(fp, callee, this, &args, caller, call_pc, info)?;
        } else {
            let top = self.frame_end();
            let info = u64::from(site.dst.0);
            self.push_frame(
                top,
                callee,
                this,
                &args,
                u64::from(self.fp),
                pc as u64,
                info,
            )?;
        }
        Ok(Called::Frame)
    }

    /// Whether a frame for template `t` with `argc` arguments fits at `fp`.
    fn fits(&self, fp: u32, t: u32, argc: u32) -> bool {
        let template = &self.templates[t as usize];
        let extra = argc.saturating_sub(template.params);
        let end = u64::from(fp)
            + u64::from(HEADER + template.limits.registers)
            + u64::from(extra)
            + u64::from(RESERVE);
        end <= u64::from(self.stack.capacity())
    }

    /// Makes the pending getter or setter call from the property op at
    /// `pc`: like a call from that instruction, with the result going to
    /// `dst` (a getter) or dropped (`None`, a setter).
    fn call_accessor(&mut self, dst: Option<Reg>, pc: usize) -> Result<Called, Thrown> {
        let p = self
            .pending
            .take()
            .expect("Flow::Accessor leaves a pending call");
        let args: &[Value] = match &p.arg {
            Some(v) => std::slice::from_ref(v),
            None => &[],
        };
        let Some(t) = self.template_of(p.callee) else {
            if let Some(i) = self.native_of(p.callee) {
                return self.call_native(i, p.this, args).map(Called::Native);
            }
            // Only functions are stored as getters and setters, so this
            // can't happen yet; Object.defineProperty (1.13) checks too.
            return Err(self.error(ErrorKind::NotCallable));
        };
        let top = self.frame_end();
        if !self.fits(top, t, args.len() as u32) {
            return Err(self.error(ErrorKind::StackOverflow));
        }
        if let Flow::Throw = self.check_interrupt() {
            return Err(Thrown);
        }
        let info = dst.map_or(DISCARD, |d| u64::from(d.0));
        self.push_frame(
            top,
            p.callee,
            p.this,
            args,
            u64::from(self.fp),
            pc as u64,
            info,
        )?;
        Ok(Called::Frame)
    }

    /// Writes a frame at `fp` for closure `callee` and makes it
    /// current; a `RangeError` (in the current frame) if it doesn't fit.
    #[allow(clippy::too_many_arguments)]
    fn push_frame(
        &mut self,
        fp: u32,
        callee: Value,
        this: Value,
        args: &[Value],
        caller: u64,
        call_pc: u64,
        info: u64,
    ) -> Result<(), Thrown> {
        let (flags, word) = function_word(self.s.nogc(), callee).expect("checked by the caller");
        debug_assert!(flags & object::CLOSURE != 0, "a closure");
        let t = word as u32;
        let env = match (word >> 32) as u32 {
            0 => Value::UNDEFINED,
            e => Value::object(HeapRef(e)),
        };
        let template = &self.templates[t as usize];
        let (regs, params) = (template.limits.registers, template.params);
        let argc = u32::try_from(args.len()).unwrap_or(u32::MAX);
        let extra = argc.saturating_sub(params);
        if !self.fits(fp, t, argc) {
            return Err(self.error(ErrorKind::StackOverflow));
        }
        let st = self.stack;
        st.put(fp + CALLEE, callee.to_bits());
        st.put(fp + THIS, this.to_bits());
        st.put(fp + NEW_TARGET, Value::UNDEFINED.to_bits());
        st.put(fp + ENV, env.to_bits());
        st.put(fp + CALLER_FP, caller);
        st.put(fp + CALL_PC, call_pc);
        st.put(fp + RETURN_INFO, info);
        st.put(fp + ARGC, u64::from(argc));
        let base = fp + HEADER;
        for r in 0..regs {
            let v = if r < params {
                args.get(r as usize).copied().unwrap_or(Value::UNDEFINED)
            } else {
                Value::UNDEFINED
            };
            st.put(base + r, v.to_bits());
        }
        for (i, a) in args.iter().skip(params as usize).enumerate() {
            st.put(base + regs + i as u32, a.to_bits());
        }
        self.fp = fp;
        self.tmpl = t;
        st.top.set(base + regs + extra);
        Ok(())
    }
}

impl OpCtx for Interp<'_, '_> {
    #[inline]
    fn reg(&self, r: Reg) -> Value {
        // Registers always hold valid values (verified code writes only
        // values; frames start filled).
        Value::from_bits_checked(self.slot(HEADER + r.0)).unwrap_or(Value::UNINITIALIZED)
    }
    #[inline]
    fn set(&mut self, r: Reg, v: Value) {
        self.set_slot(HEADER + r.0, v.to_bits());
    }
    fn constant(&self, c: Const) -> Value {
        self.template().constants[c.0 as usize]
    }
    fn note(&mut self, ic: Ic, fb: Feedback) {
        let slot = &mut self.templates[self.tmpl as usize].feedback[ic.0 as usize];
        *slot = slot.union(fb);
    }
    fn throw(&mut self, v: Value) -> Flow {
        self.last_error = None;
        self.stack.put(0, v.to_bits());
        Flow::Throw
    }
    fn check_interrupt(&mut self) -> Flow {
        if self.interrupt.load(Ordering::Relaxed) {
            self.terminating = true;
            self.stack.put(0, Value::NULL.to_bits());
            return Flow::Throw;
        }
        Flow::Next
    }

    fn rt_create_closure(&mut self, f: Func) -> Completion {
        let t = self.template().functions[f.0 as usize];
        let stack = self.stack;
        let fp = self.fp;
        let env_of = move || match Value::from_bits_checked(stack.get(fp + ENV)).map(Value::kind) {
            Some(Kind::Object(HeapRef(e))) => e,
            _ => 0,
        };
        new_function(
            self.s,
            self.tables,
            self.intrinsics,
            self.templates,
            t,
            Some(&env_of),
        )
        .map_err(|()| self.out_of_memory())
    }
    fn current_closure(&self) -> Value {
        Value::from_bits_checked(self.slot(CALLEE)).unwrap_or(Value::UNDEFINED)
    }
    fn push_env(&mut self, slots: Count) -> Result<(), Thrown> {
        self.new_env(slots.0, |_, _| Value::UNDEFINED)
    }
    fn pop_env(&mut self) -> Result<(), Thrown> {
        let nogc = self.s.nogc();
        let Some(e) = cell_of::<EnvCell>(nogc, self.env(), CellKind::Env) else {
            return self.fail();
        };
        let parent = nogc
            .read_ref::<_, EnvCell>(e, ENV_PARENT)
            .map_or(Value::UNDEFINED, |p| Value::object(HeapRef(p.offset())));
        self.set_slot(ENV, parent.to_bits());
        Ok(())
    }
    fn copy_env(&mut self) -> Result<(), Thrown> {
        let nogc = self.s.nogc();
        let Some(e) = cell_of::<EnvCell>(nogc, self.env(), CellKind::Env) else {
            return self.fail();
        };
        let count = (nogc.read_u64(e, 0) >> 32) as u32;
        let parent = nogc.read_u32(e, ENV_PARENT);
        // The old environment stays current (so it and every value in it
        // stay rooted) until the copy is allocated; `new_env` then links
        // the copy to it, and the link is moved to the old parent.
        let values: Vec<Value> = (0..count)
            .map(|i| {
                Value::from_bits_checked(nogc.read_u64(e, ENV_SLOTS + 8 * i))
                    .unwrap_or(Value::UNINITIALIZED)
            })
            .collect();
        self.new_env(count, |_, i| values[i as usize])?;
        let nogc = self.s.nogc();
        let copy = cell_of::<EnvCell>(nogc, self.env(), CellKind::Env).expect("just made");
        nogc.write_ref(copy, ENV_PARENT, nogc.handle::<EnvCell>(parent));
        Ok(())
    }
    fn env_value(&self) -> Value {
        self.env()
    }
    fn set_env(&mut self, v: Value) -> Result<(), Thrown> {
        if v.is_undefined() || cell_of::<EnvCell>(self.s.nogc(), v, CellKind::Env).is_some() {
            self.set_slot(ENV, v.to_bits());
            Ok(())
        } else {
            self.fail()
        }
    }
    fn env_get(&mut self, slot: EnvSlot) -> Completion {
        match self.env_cell(slot) {
            Some((e, field)) => Ok(Value::from_bits_checked(self.s.nogc().read_u64(e, field))
                .unwrap_or(Value::UNINITIALIZED)),
            None => self.fail(),
        }
    }
    fn env_set(&mut self, slot: EnvSlot, v: Value) -> Result<(), Thrown> {
        match self.env_cell(slot) {
            Some((e, field)) => {
                store(self.s.nogc(), e, field, v);
                Ok(())
            }
            None => self.fail(),
        }
    }

    fn rt_throw_error(&mut self, e: ErrorKind) -> Flow {
        self.error(e);
        Flow::Throw
    }
    fn this_value(&self) -> Value {
        Value::from_bits_checked(self.slot(THIS)).unwrap_or(Value::UNDEFINED)
    }
    fn rt_load_name(&mut self, name: Value) -> Completion {
        let (key, units) = self.name_key(name)?;
        if let Some(b) = self.lexical(&units) {
            let v = self.lexical_value(b);
            if v.is_uninitialized() {
                return Err(self.error(ErrorKind::UninitializedBinding));
            }
            return Ok(v);
        }
        match self.global_own(key) {
            Some((PropertyValue::Data(v), _)) => Ok(v),
            // Calling getters comes with task 1.8e.
            Some((PropertyValue::Accessor { .. }, _)) => self.fail(),
            None => Err(self.error(ErrorKind::UnresolvableReference)),
        }
    }
    fn rt_store_name(&mut self, name: Value, v: Value, strict: bool) -> Result<(), Thrown> {
        let (key, units) = self.name_key(name)?;
        if let Some(b) = self.lexical(&units) {
            if self.lexical_value(b).is_uninitialized() {
                return Err(self.error(ErrorKind::UninitializedBinding));
            }
            if b.constant {
                return Err(self.error(ErrorKind::ConstAssignment));
            }
            if v.is_uninitialized() {
                return self.fail();
            }
            self.lexical_set(b.slot, v);
            return Ok(());
        }
        if strict && self.global_own(key).is_none() {
            return Err(self.error(ErrorKind::UnresolvableReference));
        }
        // Sloppy code creates a configurable property (an implicit
        // global).
        self.global_put(key, v, None)
    }
    fn rt_declare_global(&mut self, name: Value, kind: GlobalKind) -> Result<(), Thrown> {
        let (key, units) = self.name_key(name)?;
        let lexical = self.lexical(&units).is_some();
        match kind {
            GlobalKind::Var => {
                if lexical {
                    return Err(self.error(ErrorKind::GlobalRedeclaration));
                }
                // CreateGlobalVarBinding: an existing property (an
                // implicit global, say) stays as it is.
                if self.global_own(key).is_none() {
                    let attrs = Attrs(Attrs::WRITABLE.0 | Attrs::ENUMERABLE.0);
                    self.global_put(key, Value::UNDEFINED, Some(attrs))?;
                }
                Ok(())
            }
            GlobalKind::Let | GlobalKind::Const => {
                // HasRestrictedGlobalProperty: a non-configurable property
                // (a declared var) can't be shadowed; an implicit global
                // can.
                let restricted = self
                    .global_own(key)
                    .is_some_and(|(_, a)| !a.contains(Attrs::CONFIGURABLE));
                if lexical || restricted {
                    return Err(self.error(ErrorKind::GlobalRedeclaration));
                }
                self.lexical_add(units, kind == GlobalKind::Const)
            }
        }
    }
    fn rt_init_global(&mut self, name: Value, v: Value) -> Result<(), Thrown> {
        let (_, units) = self.name_key(name)?;
        match self.lexical(&units) {
            Some(b) if !v.is_uninitialized() => {
                self.lexical_set(b.slot, v);
                Ok(())
            }
            _ => self.fail(),
        }
    }

    fn rt_add(&mut self, a: Value, b: Value) -> Completion {
        if a.is_string() || b.is_string() {
            // Check the length before copying anything (a number or other
            // primitive's text is short).
            let len = |v: Value| self.str_len(v).map_or(0, |l| l as usize);
            if len(a) + len(b) > MAX_STRING_UNITS {
                return self.fail();
            }
            return match (self.text(a), self.text(b)) {
                (Some(mut x), Some(y)) => {
                    x.extend_from_slice(&y);
                    self.make_string(&x)
                }
                _ => self.fail(),
            };
        }
        self.numbers(a, b, |x, y| x + y)
    }
    fn rt_sub(&mut self, a: Value, b: Value) -> Completion {
        self.numbers(a, b, |x, y| x - y)
    }
    fn rt_mul(&mut self, a: Value, b: Value) -> Completion {
        self.numbers(a, b, |x, y| x * y)
    }
    fn rt_less_than(&mut self, a: Value, b: Value) -> Completion {
        if let Some(o) = self.string_order(a, b) {
            return Ok(Value::from_bool(o.is_lt()));
        }
        self.predicate(a, b, |x, y| x < y)
    }
    fn rt_strict_equal(&mut self, a: Value, b: Value) -> bool {
        // Numbers are handled inline. Strings compare by contents (only
        // atoms are interned); everything else by identity.
        a == b || self.same_string(a, b)
    }
    fn rt_numeric(&mut self, op: NumericOp, a: Value, b: Value) -> Completion {
        self.numbers(a, b, |x, y| number::numeric(op, x, y))
    }
    fn rt_compare(&mut self, op: CompareOp, a: Value, b: Value) -> Completion {
        if let Some(o) = self.string_order(a, b) {
            return Ok(Value::from_bool(match op {
                CompareOp::Greater => o.is_gt(),
                CompareOp::LessEq => o.is_le(),
                CompareOp::GreaterEq => o.is_ge(),
            }));
        }
        self.predicate(a, b, |x, y| number::compare(op, x, y))
    }
    fn rt_unary(&mut self, op: UnaryOp, a: Value) -> Completion {
        match self.num(a) {
            Some(x) => Ok(Value::number(number::unary(op, x))),
            None => self.fail(),
        }
    }
    fn rt_loose_equal(&mut self, a: Value, b: Value) -> Completion {
        if a.is_nullish() || b.is_nullish() {
            return Ok(Value::from_bool(a.is_nullish() && b.is_nullish()));
        }
        // Two strings compare by contents, two cells by identity; an
        // object against a primitive needs ToPrimitive (with the
        // built-ins); any other mix compares as numbers.
        if a.is_string() && b.is_string() {
            return Ok(Value::from_bool(self.same_string(a, b)));
        }
        let object = |v: Value| matches!(v.kind(), Kind::Object(_));
        if object(a) && object(b) {
            return Ok(Value::from_bool(a == b));
        }
        self.predicate(a, b, |x, y| x == y)
    }
    fn rt_to_boolean(&mut self, v: Value) -> bool {
        match v.kind() {
            Kind::Undefined | Kind::Null => false,
            Kind::Bool(b) => b,
            Kind::Int32(i) => i != 0,
            Kind::Double(d) => !(d == 0.0 || d.is_nan()),
            Kind::String(HeapRef(r)) => {
                let nogc = self.s.nogc();
                nogc.handle::<crate::JsString>(r)
                    .is_some_and(|s| string::len(nogc, s) > 0)
            }
            Kind::Object(_) | Kind::Other(..) | Kind::Uninitialized => true,
        }
    }
    fn rt_typeof(&mut self, v: Value) -> Value {
        let name = match v.kind() {
            Kind::Undefined | Kind::Uninitialized => TypeName::Undefined,
            Kind::Null => TypeName::Object,
            Kind::Bool(_) => TypeName::Boolean,
            Kind::Int32(_) | Kind::Double(_) => TypeName::Number,
            Kind::String(_) => TypeName::String,
            Kind::Object(_) if function_word(self.s.nogc(), v).is_some() => TypeName::Function,
            Kind::Object(_) => TypeName::Object,
            Kind::Other(ferrojs_value::OtherKind::Symbol, _) => TypeName::Symbol,
            Kind::Other(ferrojs_value::OtherKind::BigInt, _) => TypeName::Bigint,
        };
        self.type_names[name as usize]
    }
    fn rt_typeof_name(&mut self, name: Value) -> Completion {
        let (key, units) = self.name_key(name)?;
        let v = match self.lexical(&units) {
            Some(b) => {
                let v = self.lexical_value(b);
                if v.is_uninitialized() {
                    return Err(self.error(ErrorKind::UninitializedBinding));
                }
                v
            }
            None => match self.global_own(key) {
                None => Value::UNDEFINED,
                Some((PropertyValue::Data(v), _)) => v,
                Some((PropertyValue::Accessor { .. }, _)) => return self.fail(),
            },
        };
        Ok(self.rt_typeof(v))
    }
    fn rt_to_string(&mut self, v: Value) -> Completion {
        match self.text(v) {
            Some(units) => self.make_string(&units),
            None => self.fail(),
        }
    }

    fn rt_new_object(&mut self) -> Completion {
        let tables = &mut *self.tables;
        let intr = self.intrinsics;
        let made = self.s.scope(|n| {
            let proto = n.root(intr.object_prototype.get(n.nogc()));
            object::new_object(n, tables, Some(proto), object::DEFAULT_INLINE)
                .map(|o| o.bind(n.nogc()).offset())
        });
        match made {
            Ok(o) => Ok(Value::object(HeapRef(o))),
            Err(_) => Err(self.out_of_memory()),
        }
    }
    fn rt_get_property(&mut self, obj: Value, key: Value) -> Access {
        if obj.is_nullish() {
            return Err(self.error(ErrorKind::NullishBase).into());
        }
        let key = self.key(key)?;
        let nogc = self.s.nogc();
        match (self.base(obj), &key) {
            (Base::Object(o), Key::Index(i)) => Ok(elements::get(nogc, o, *i)),
            (Base::Object(o), Key::Name(units)) => {
                if is_length(units) && elements::is_array(nogc, o) {
                    return Ok(Value::number(f64::from(elements::length(nogc, o))));
                }
                let found = self.find_atom(units).and_then(|k| {
                    let mut cur = Some(o.cast::<Object>());
                    while let Some(obj) = cur {
                        if let Some((v, _)) = object::get_own(nogc, obj, k) {
                            return Some(v);
                        }
                        cur = object::prototype(nogc, obj);
                    }
                    None
                });
                match found {
                    Some(PropertyValue::Data(v)) => Ok(v),
                    Some(PropertyValue::Accessor { get, .. }) if get.is_undefined() => {
                        Ok(Value::UNDEFINED)
                    }
                    Some(PropertyValue::Accessor { get, .. }) => {
                        // The receiver is the base, wherever the getter was
                        // found.
                        self.pending = Some(PendingCall {
                            callee: get,
                            this: obj,
                            arg: None,
                        });
                        Err(Detour::Call)
                    }
                    // Inherited from Object.prototype or Function.prototype
                    // once their methods exist (1.13).
                    None if missing_builtin(nogc, obj, units) => Ok(self.fail()?),
                    None => Ok(Value::UNDEFINED),
                }
            }
            (Base::String(_, len), Key::Name(units)) if is_length(units) => {
                Ok(Value::number(f64::from(len)))
            }
            (Base::String(s, len), Key::Index(i)) => {
                if *i >= len {
                    // Nothing on String.prototype has an index key.
                    return Ok(Value::UNDEFINED);
                }
                let unit = string::units(nogc, s)[*i as usize];
                Ok(self.make_string(&[unit])?)
            }
            // Other keys on strings, numbers and booleans need their
            // prototypes (1.13).
            _ => Ok(self.fail()?),
        }
    }
    fn rt_set_property(
        &mut self,
        obj: Value,
        key: Value,
        v: Value,
        strict: bool,
    ) -> Result<(), Detour> {
        if obj.is_nullish() {
            return Err(self.error(ErrorKind::NullishBase).into());
        }
        let key = self.key(key)?;
        if matches!(&key, Key::Name(u) if u.iter().copied().eq("__proto__".encode_utf16())) {
            // The Object.prototype accessor sets the prototype (1.13).
            return Ok(self.fail()?);
        }
        let ok = match self.base(obj) {
            Base::Object(_) => match self.put(obj, key, v, false)? {
                SetResult::Done(ok) => ok,
                SetResult::Setter(f) => {
                    // The assignment succeeds whatever the setter returns.
                    self.pending = Some(PendingCall {
                        callee: f,
                        this: obj,
                        arg: Some(v),
                    });
                    return Err(Detour::Call);
                }
            },
            // The wrapper object is the receiver: OrdinarySet refuses (no
            // setters on the prototypes yet).
            Base::String(..) | Base::Primitive => false,
            Base::Nullish | Base::Other => return Ok(self.fail()?),
        };
        if !ok && strict {
            return Err(self.error(ErrorKind::PropertyRefused).into());
        }
        Ok(())
    }
    fn rt_define_property(&mut self, obj: Value, key: Value, v: Value) -> Result<(), Thrown> {
        let key = self.key(key)?;
        if !matches!(self.base(obj), Base::Object(_)) {
            return self.fail();
        }
        if self.put(obj, key, v, true)? == SetResult::Done(true) {
            Ok(())
        } else {
            // Literals define on fresh extensible objects: can't refuse.
            self.fail()
        }
    }
    fn rt_define_accessor(
        &mut self,
        obj: Value,
        key: Value,
        f: Value,
        setter: bool,
    ) -> Result<(), Thrown> {
        let key = self.key(key)?;
        let Base::Object(_) = self.base(obj) else {
            return self.fail();
        };
        let Key::Name(units) = key else {
            // The elements store has no accessors yet (D-040).
            return self.fail();
        };
        let tables = &mut *self.tables;
        let done = self.s.scope(|n| {
            let nogc = n.nogc();
            let o = cell_of::<Object>(nogc, obj, CellKind::Object).expect("checked above");
            let o = n.root(o);
            let Some(f) = rooted(n, f) else {
                return Ok(false);
            };
            let k = string::atom_units(n, tables, &units)?.bind(n.nogc());
            let k = n.root(k);
            let half = Some(f);
            let desc = Descriptor {
                get: if setter { None } else { half },
                set: if setter { half } else { None },
                enumerable: Some(true),
                configurable: Some(true),
                ..Descriptor::default()
            };
            object::define_property(n, o, k, desc)
        });
        match done {
            Ok(true) => Ok(()),
            // Literals define on fresh extensible objects: can't refuse.
            Ok(false) => self.fail(),
            Err(_) => Err(self.out_of_memory()),
        }
    }
    fn rt_delete_property(&mut self, obj: Value, key: Value, strict: bool) -> Completion {
        if obj.is_nullish() {
            return Err(self.error(ErrorKind::NullishBase));
        }
        let key = self.key(key)?;
        let gone = match (self.base(obj), key) {
            (Base::Object(o), Key::Index(i)) => elements::delete(self.s.nogc(), o, i),
            (Base::Object(o), Key::Name(units)) => {
                let nogc = self.s.nogc();
                if is_length(&units) && elements::is_array(nogc, o) {
                    false
                } else {
                    match self.find_atom(&units).map(|k| k.offset()) {
                        None => true,
                        Some(k) => {
                            let o = o.offset();
                            let done = self.s.scope(|n| {
                                let nogc = n.nogc();
                                let o = n.root(nogc.handle::<Object>(o).expect("a live object"));
                                let k = n.root(nogc.handle::<JsString>(k).expect("a live atom"));
                                object::delete(n, o, k)
                            });
                            match done {
                                Ok(gone) => gone,
                                Err(_) => return Err(self.out_of_memory()),
                            }
                        }
                    }
                }
            }
            (Base::String(_, len), Key::Index(i)) => i >= len,
            (Base::String(..), Key::Name(units)) => !is_length(&units),
            (Base::Primitive, _) => true,
            (Base::Nullish | Base::Other, _) => return self.fail(),
        };
        if !gone && strict {
            return Err(self.error(ErrorKind::PropertyRefused));
        }
        Ok(Value::from_bool(gone))
    }
    fn rt_has_property(&mut self, key: Value, obj: Value) -> Completion {
        match self.base(obj) {
            Base::Object(_) => {}
            Base::Other => return self.fail(),
            _ => return Err(self.error(ErrorKind::NotAnObject)),
        }
        let key = self.key(key)?;
        let Base::Object(o) = self.base(obj) else {
            unreachable!("checked above")
        };
        let has = self.has(o, &key);
        if !has && matches!(&key, Key::Name(u) if missing_builtin(self.s.nogc(), obj, u)) {
            // Inherited from Object.prototype or Function.prototype once
            // their methods exist (1.13).
            return self.fail();
        }
        Ok(Value::from_bool(has))
    }
}

/// Names of `Function.prototype`'s properties that don't exist yet
/// (1.13); a function missing one of them must fail cleanly, not read
/// `undefined`.
const FUNCTION_PROTOTYPE_NAMES: [&str; 6] =
    ["apply", "arguments", "bind", "call", "caller", "toString"];

/// Whether a missing named property `units` of `base` would be found on
/// a built-in prototype once the built-ins exist (1.13): looking it up
/// must fail cleanly instead of answering `undefined` or `false`.
fn missing_builtin(nogc: NoGc<'_>, base: Value, units: &[u16]) -> bool {
    number::is_object_prototype_name(units)
        || (function_word(nogc, base).is_some()
            && FUNCTION_PROTOTYPE_NAMES
                .iter()
                .any(|n| n.encode_utf16().eq(units.iter().copied())))
}

/// Whether `units` spell `length`.
fn is_length(units: &[u16]) -> bool {
    units.iter().copied().eq("length".encode_utf16())
}
