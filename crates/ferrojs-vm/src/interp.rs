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
//!   stack slot; nothing else needs rooting.
//!
//! Strings beyond constants, globals other than an empty global object,
//! and error objects come with 1.7c and 1.13: until then a thrown error is
//! `undefined` with its [`ErrorKind`] recorded, as in the toy context.

use std::cell::Cell;
use std::fmt;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use ferrojs_bytecode::number::{self, CompareOp, NumericOp, UnaryOp};
use ferrojs_bytecode::{
    decode, execute, verify, verify_handlers, CallSite, Completion, Const, Count, EnvSlot,
    ErrorKind, Feedback, Flow, Func, GlobalKind, Handler, Ic, Limits, OpCtx, Reg, Thrown,
    VerifyError,
};
use ferrojs_compiler::{CompiledFunction, Constant};
use ferrojs_core::gc::{Global, Scope};
use ferrojs_core::{Gc, NoGc};
use ferrojs_value::{HeapRef, Kind, Value};

use crate::{string, value_ref, CellKind, Tables, Vm};

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

// Frame header slots (fp + n).
const CALLEE: u32 = 0;
const THIS: u32 = 1;
const NEW_TARGET: u32 = 2;
const ENV: u32 = 3;
const CALLER_FP: u32 = 4;
/// The caller's pc of the call instruction (the resume point is the
/// instruction after it; unwinding looks up handlers at the call).
const CALL_PC: u32 = 5;
const RETURN_INFO: u32 = 6;
const ARGC: u32 = 7;

// Function cell: header (kind | template << 32), environment reference.
/// Offset of a function cell's environment reference.
pub const FUNCTION_ENV: u32 = 8;
const FUNCTION_SIZE: u32 = 16;
// Env cell: header (kind | slot count << 32), parent reference, slots.
/// Offset of an environment's parent reference.
pub const ENV_PARENT: u32 = 8;
/// Offset of an environment's first slot.
pub const ENV_SLOTS: u32 = 16;

/// Cell type markers.
pub struct FunctionCell;
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
    pub code: Vec<u8>,
    pub handlers: Vec<Handler>,
    /// Constants as values; strings are atoms (kept alive by the atom
    /// table).
    pub constants: Vec<Value>,
    pub limits: Limits,
    pub params: u32,
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
}

impl State {
    pub(crate) fn new(stack: Rc<FrameStack>) -> State {
        State {
            stack,
            anchor: None,
            templates: Vec::new(),
            type_names: Vec::new(),
        }
    }
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
            let Ok(callee) = new_function(s, t, None) else {
                return Outcome::OutOfMemory;
            };
            let stack = interp.stack.clone();
            let base_top = stack.top.get();
            let mut it = Interp {
                s,
                stack: &stack,
                templates: &mut interp.templates,
                type_names: &interp.type_names,
                interrupt: interrupt.as_ref(),
                fp: 0,
                tmpl: t,
                last_error: None,
                terminating: false,
                oom: false,
            };
            let out = it.run(callee, this, args);
            stack.top.set(base_top);
            out
        })
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
    let index = templates.len() as u32;
    templates.push(Template {
        name: f.name.clone(),
        code: f.code.clone(),
        handlers: f.handlers.clone(),
        constants,
        limits,
        params: f.params,
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

/// Makes the stack anchor and the `typeof` atoms on first use.
fn prepare(s: &mut Scope<'_>, tables: &mut Tables, st: &mut State) -> Result<(), ()> {
    if st.anchor.is_none() {
        let a = s.alloc::<StackAnchor>(8).map_err(|_| ())?.bind(s.nogc());
        s.nogc().write_u64(a, 0, CellKind::Stack as u64);
        st.anchor = Some(s.nogc().global(a));
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

/// A new function cell; `env` is an environment cell offset (rooted by
/// the caller: it is read again after the allocation).
fn new_function(s: &mut Scope<'_>, t: u32, env: Option<&dyn Fn() -> u32>) -> Result<Value, ()> {
    let f = s
        .alloc::<FunctionCell>(FUNCTION_SIZE)
        .map_err(|_| ())?
        .bind(s.nogc());
    let nogc = s.nogc();
    nogc.write_u64(f, 0, CellKind::Function as u64 | (u64::from(t) << 32));
    let env = env.map_or(0, |e| e());
    nogc.write_ref(f, FUNCTION_ENV, nogc.handle::<EnvCell>(env));
    Ok(Value::object(HeapRef(f.offset())))
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
    interrupt: &'r std::sync::atomic::AtomicBool,
    fp: u32,
    tmpl: u32,
    last_error: Option<ErrorKind>,
    terminating: bool,
    oom: bool,
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

    /// The end of the current frame: registers, then extra arguments.
    fn frame_end(&self) -> u32 {
        let t = self.template();
        let extra = (self.slot(ARGC) as u32).saturating_sub(t.params);
        self.fp + HEADER + t.limits.registers + extra
    }

    fn env(&self) -> Value {
        Value::from_bits_checked(self.slot(ENV)).unwrap_or(Value::UNDEFINED)
    }

    /// The template of the function cell `callee` (a value in a frame).
    fn template_of(&self, callee: Value) -> Option<u32> {
        let nogc = self.s.nogc();
        let f = cell_of::<FunctionCell>(nogc, callee, CellKind::Function)?;
        Some((nogc.read_u64(f, 0) >> 32) as u32)
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
        match (to_number(a), to_number(b)) {
            (Some(x), Some(y)) => Ok(Value::number(f(x, y))),
            _ => self.fail(),
        }
    }

    fn predicate(&mut self, a: Value, b: Value, f: impl FnOnce(f64, f64) -> bool) -> Completion {
        match (to_number(a), to_number(b)) {
            (Some(x), Some(y)) => Ok(Value::from_bool(f(x, y))),
            _ => self.fail(),
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
        loop {
            let t = &self.templates[self.tmpl as usize];
            let (insn, len) = decode(&t.code, pc).expect("verified bytecode decodes");
            let flow = match execute(self, insn) {
                Flow::Call => {
                    let site = CallSite::of(&insn).expect("only calls return Flow::Call");
                    match self.call(site, pc) {
                        Ok(()) => {
                            pc = 0;
                            continue;
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
                    let dst = Reg(self.slot(RETURN_INFO) as u32);
                    self.fp = caller as u32;
                    self.tmpl = self
                        .template_of(Value::from_bits_checked(self.slot(CALLEE)).expect("callee"))
                        .expect("a caller frame runs a function");
                    self.stack.top.set(self.frame_end());
                    let code = &self.templates[self.tmpl as usize].code;
                    let (_, call_len) = decode(code, call_pc).expect("verified call");
                    pc = call_pc + call_len;
                    self.set(dst, v);
                }
                Flow::Throw => match self.unwind(pc) {
                    Some(target) => pc = target,
                    None => return self.uncaught(),
                },
                Flow::Call => unreachable!("handled above"),
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
    fn call(&mut self, site: CallSite, pc: usize) -> Result<(), Thrown> {
        let callee = self.reg(site.callee);
        let Some(t) = self.template_of(callee) else {
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
            self.push_frame(fp, callee, this, &args, caller, call_pc, info)
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
            )
        }
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

    /// Writes a frame at `fp` for function cell `callee` and makes it
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
        let t = self.template_of(callee).expect("checked by the caller");
        let nogc = self.s.nogc();
        let f = cell_of::<FunctionCell>(nogc, callee, CellKind::Function).expect("a function");
        let env = match nogc.read_u32(f, FUNCTION_ENV) {
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
        new_function(self.s, t, Some(&env_of)).map_err(|()| self.out_of_memory())
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
    // The global object is empty until 1.7c.
    fn rt_load_name(&mut self, _name: Value) -> Completion {
        Err(self.error(ErrorKind::UnresolvableReference))
    }
    fn rt_store_name(&mut self, _name: Value, _v: Value, strict: bool) -> Result<(), Thrown> {
        if strict {
            Err(self.error(ErrorKind::UnresolvableReference))
        } else {
            self.fail()
        }
    }
    fn rt_declare_global(&mut self, _name: Value, _kind: GlobalKind) -> Result<(), Thrown> {
        self.fail()
    }
    fn rt_init_global(&mut self, _name: Value, _v: Value) -> Result<(), Thrown> {
        self.fail()
    }

    fn rt_add(&mut self, a: Value, b: Value) -> Completion {
        self.numbers(a, b, |x, y| x + y)
    }
    fn rt_sub(&mut self, a: Value, b: Value) -> Completion {
        self.numbers(a, b, |x, y| x - y)
    }
    fn rt_mul(&mut self, a: Value, b: Value) -> Completion {
        self.numbers(a, b, |x, y| x * y)
    }
    fn rt_less_than(&mut self, a: Value, b: Value) -> Completion {
        self.predicate(a, b, |x, y| x < y)
    }
    fn rt_strict_equal(&mut self, a: Value, b: Value) -> bool {
        // Atoms and cells compare by identity; numbers are handled inline.
        a == b
    }
    fn rt_numeric(&mut self, op: NumericOp, a: Value, b: Value) -> Completion {
        self.numbers(a, b, |x, y| number::numeric(op, x, y))
    }
    fn rt_compare(&mut self, op: CompareOp, a: Value, b: Value) -> Completion {
        self.predicate(a, b, |x, y| number::compare(op, x, y))
    }
    fn rt_unary(&mut self, op: UnaryOp, a: Value) -> Completion {
        match to_number(a) {
            Some(x) => Ok(Value::number(number::unary(op, x))),
            None => self.fail(),
        }
    }
    fn rt_loose_equal(&mut self, a: Value, b: Value) -> Completion {
        if a.is_nullish() || b.is_nullish() {
            return Ok(Value::from_bool(a.is_nullish() && b.is_nullish()));
        }
        // Two cells, or two strings (only atoms exist so far, and atoms
        // are interned), compare by identity.
        let object = |v: Value| matches!(v.kind(), Kind::Object(_));
        if (object(a) && object(b)) || (a.is_string() && b.is_string()) {
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
            Kind::Object(_) if self.template_of(v).is_some() => TypeName::Function,
            Kind::Object(_) => TypeName::Object,
            Kind::Other(ferrojs_value::OtherKind::Symbol, _) => TypeName::Symbol,
            Kind::Other(ferrojs_value::OtherKind::BigInt, _) => TypeName::Bigint,
        };
        self.type_names[name as usize]
    }
    fn rt_typeof_name(&mut self, _name: Value) -> Completion {
        // Nothing is declared in the (empty) global object yet.
        Ok(self.type_names[TypeName::Undefined as usize])
    }
    fn rt_to_string(&mut self, _v: Value) -> Completion {
        self.fail()
    }
}
