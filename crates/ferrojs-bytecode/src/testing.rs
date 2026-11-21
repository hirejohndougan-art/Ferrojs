//! A reference context and dispatch loop over plain values, for tests,
//! benchmarks and fuzzing of the op definitions and the compiler before
//! the real interpreter (task 1.7) exists. Primitives only: the slow paths
//! convert `undefined`, `null`, booleans and numbers as ToNumber does, and
//! throw `undefined` for heap values (strings, objects, symbols), which the
//! toy can't look inside. Functions, closures and environments live in
//! vectors; [`run_program`] runs calls with a frame stack, the way the
//! interpreter will (ARCHITECTURE.md §3.3–3.4), without Rust recursion.

use ferrojs_value::{HeapRef, Kind, Value};

use crate::number::{self, CompareOp, NumericOp, UnaryOp};
use crate::{
    decode, execute, CallSite, Completion, Const, Count, EnvSlot, ErrorKind, Feedback, Flow, Func,
    Handler, Ic, Limits, OpCtx, Reg, Thrown,
};

/// Toy closures are object values with this bit set in the reference.
const CLOSURE_BIT: u32 = 1 << 30;
/// Toy environments are object values with this bit set.
const ENV_BIT: u32 = 1 << 29;
/// The most slots one toy environment may have (fuzzed code can ask for
/// 2^32).
const MAX_ENV_SLOTS: u32 = 1 << 16;

/// One function of a program for [`run_program`].
#[derive(Clone, Debug, Default)]
pub struct ToyFunction {
    pub code: Vec<u8>,
    pub handlers: Vec<Handler>,
    pub constants: Vec<Value>,
    pub limits: Limits,
    /// Declared parameter count: arguments land in `r0..params`.
    pub params: u32,
    /// The program index of each nested function (`Func` operands).
    pub functions: Vec<usize>,
}

#[derive(Clone, Debug)]
struct ToyEnv {
    parent: Option<usize>,
    slots: Vec<Value>,
}

#[derive(Clone, Copy, Debug)]
struct ToyClosure {
    func: usize,
    env: Option<usize>,
}

/// Registers, constants, feedback, environments and closures in vectors.
#[derive(Debug, Default)]
pub struct ToyCtx {
    pub regs: Vec<Value>,
    pub constants: Vec<Value>,
    pub feedback: Vec<Feedback>,
    pub exception: Option<Value>,
    /// The interrupt check terminates once this many checks have passed.
    pub interrupt_after: Option<u64>,
    pub interrupt_checks: u64,
    /// How many times each slow path ran (for tests of the fast paths).
    pub slow_calls: u64,
    /// Set when the interrupt check terminated execution: termination is
    /// not catchable, so the run loops don't unwind into handlers then.
    pub terminating: bool,
    /// The kind of the last error a runtime function threw, if any (the
    /// toy has no error objects; the exception value is `undefined`).
    pub last_error: Option<ErrorKind>,
    /// Frames [`run_program`] may stack before a `RangeError` (default
    /// 10,000 when zero).
    pub max_depth: usize,
    env: Option<usize>,
    env_store: Vec<ToyEnv>,
    closures: Vec<ToyClosure>,
    closure: Option<Value>,
    /// The running function's nested-function table.
    functions: Vec<usize>,
}

impl ToyCtx {
    /// A context sized for `limits`, registers filled with `undefined`.
    pub fn new(limits: &Limits) -> ToyCtx {
        ToyCtx {
            regs: vec![Value::UNDEFINED; limits.registers as usize],
            constants: vec![Value::UNDEFINED; limits.constants as usize],
            feedback: vec![Feedback::NONE; limits.ics as usize],
            ..ToyCtx::default()
        }
    }

    /// Makes `chain` (outermost first) the current environment chain.
    pub fn set_env_chain(&mut self, chain: Vec<Vec<Value>>) {
        self.env = None;
        for slots in chain {
            self.env_store.push(ToyEnv {
                parent: self.env,
                slots,
            });
            self.env = Some(self.env_store.len() - 1);
        }
    }

    /// The slots of the environment `depth` levels out from the current one.
    pub fn env_slots(&self, depth: usize) -> Option<&[Value]> {
        let mut e = self.env?;
        for _ in 0..depth {
            e = self.env_store[e].parent?;
        }
        Some(&self.env_store[e].slots)
    }

    fn env_index(&self, slot: EnvSlot) -> Option<(usize, usize)> {
        let mut e = self.env?;
        for _ in 0..slot.depth {
            e = self.env_store[e].parent?;
        }
        let s = slot.slot as usize;
        (s < self.env_store[e].slots.len()).then_some((e, s))
    }

    fn closure_of(&self, v: Value) -> Option<ToyClosure> {
        match v.kind() {
            Kind::Object(HeapRef(r)) if r & CLOSURE_BIT != 0 => {
                self.closures.get((r & !CLOSURE_BIT) as usize).copied()
            }
            _ => None,
        }
    }

    /// ToNumber for the primitives the toy understands.
    pub fn to_number(v: Value) -> Option<f64> {
        match v.kind() {
            Kind::Undefined => Some(f64::NAN),
            Kind::Null => Some(0.0),
            Kind::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
            Kind::Int32(i) => Some(f64::from(i)),
            Kind::Double(d) => Some(d),
            // Verified but arbitrary bytecode (the fuzz target) can feed the
            // TDZ marker to anything; the toy treats it like a heap value.
            Kind::Object(_) | Kind::String(_) | Kind::Other(..) | Kind::Uninitialized => None,
        }
    }

    fn fail<T>(&mut self) -> Result<T, Thrown> {
        self.last_error = None;
        self.exception = Some(Value::UNDEFINED);
        Err(Thrown)
    }

    fn numbers(&mut self, a: Value, b: Value, f: impl FnOnce(f64, f64) -> f64) -> Completion {
        self.slow_calls += 1;
        match (Self::to_number(a), Self::to_number(b)) {
            (Some(x), Some(y)) => Ok(Value::number(f(x, y))),
            _ => self.fail(),
        }
    }

    fn predicate(&mut self, a: Value, b: Value, f: impl FnOnce(f64, f64) -> bool) -> Completion {
        self.slow_calls += 1;
        match (Self::to_number(a), Self::to_number(b)) {
            (Some(x), Some(y)) => Ok(Value::from_bool(f(x, y))),
            _ => self.fail(),
        }
    }
}

impl OpCtx for ToyCtx {
    fn reg(&self, r: Reg) -> Value {
        self.regs[r.0 as usize]
    }
    fn set(&mut self, r: Reg, v: Value) {
        self.regs[r.0 as usize] = v;
    }
    fn constant(&self, c: Const) -> Value {
        self.constants[c.0 as usize]
    }
    fn note(&mut self, ic: Ic, fb: Feedback) {
        let slot = &mut self.feedback[ic.0 as usize];
        *slot = slot.union(fb);
    }
    fn throw(&mut self, v: Value) -> Flow {
        self.last_error = None;
        self.exception = Some(v);
        Flow::Throw
    }
    fn check_interrupt(&mut self) -> Flow {
        if self.interrupt_after == Some(self.interrupt_checks) {
            self.terminating = true;
            self.exception = Some(Value::NULL);
            return Flow::Throw;
        }
        self.interrupt_checks += 1;
        Flow::Next
    }
    fn env_get(&mut self, slot: EnvSlot) -> Completion {
        match self.env_index(slot) {
            Some((e, s)) => Ok(self.env_store[e].slots[s]),
            None => {
                self.exception = Some(Value::UNDEFINED);
                Err(Thrown)
            }
        }
    }
    fn env_set(&mut self, slot: EnvSlot, v: Value) -> Result<(), Thrown> {
        match self.env_index(slot) {
            Some((e, s)) => {
                self.env_store[e].slots[s] = v;
                Ok(())
            }
            None => {
                self.exception = Some(Value::UNDEFINED);
                Err(Thrown)
            }
        }
    }
    fn rt_create_closure(&mut self, f: Func) -> Completion {
        let Some(&func) = self.functions.get(f.0 as usize) else {
            return self.fail();
        };
        let i = u32::try_from(self.closures.len()).unwrap_or(u32::MAX);
        if i >= ENV_BIT {
            return self.fail();
        }
        self.closures.push(ToyClosure {
            func,
            env: self.env,
        });
        Ok(Value::object(HeapRef(CLOSURE_BIT | i)))
    }
    fn current_closure(&self) -> Value {
        self.closure.unwrap_or(Value::UNDEFINED)
    }
    fn push_env(&mut self, slots: Count) -> Result<(), Thrown> {
        if slots.0 > MAX_ENV_SLOTS || self.env_store.len() >= ENV_BIT as usize {
            return self.fail();
        }
        self.env_store.push(ToyEnv {
            parent: self.env,
            slots: vec![Value::UNDEFINED; slots.0 as usize],
        });
        self.env = Some(self.env_store.len() - 1);
        Ok(())
    }
    fn pop_env(&mut self) -> Result<(), Thrown> {
        match self.env {
            Some(e) => {
                self.env = self.env_store[e].parent;
                Ok(())
            }
            None => self.fail(),
        }
    }
    fn copy_env(&mut self) -> Result<(), Thrown> {
        match self.env {
            Some(e) if self.env_store.len() < ENV_BIT as usize => {
                let copy = self.env_store[e].clone();
                self.env_store.push(copy);
                self.env = Some(self.env_store.len() - 1);
                Ok(())
            }
            _ => self.fail(),
        }
    }
    fn env_value(&self) -> Value {
        match self.env {
            Some(e) => Value::object(HeapRef(ENV_BIT | e as u32)),
            None => Value::UNDEFINED,
        }
    }
    fn set_env(&mut self, v: Value) -> Result<(), Thrown> {
        match v.kind() {
            Kind::Undefined => {
                self.env = None;
                Ok(())
            }
            Kind::Object(HeapRef(r)) if r & ENV_BIT != 0 && r & CLOSURE_BIT == 0 => {
                let e = (r & !ENV_BIT) as usize;
                if e < self.env_store.len() {
                    self.env = Some(e);
                    Ok(())
                } else {
                    self.fail()
                }
            }
            _ => self.fail(),
        }
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
    fn rt_throw_error(&mut self, e: ErrorKind) -> Flow {
        self.last_error = Some(e);
        self.exception = Some(Value::UNDEFINED);
        Flow::Throw
    }
    fn rt_less_than(&mut self, a: Value, b: Value) -> Completion {
        self.predicate(a, b, |x, y| x < y)
    }
    fn rt_numeric(&mut self, op: NumericOp, a: Value, b: Value) -> Completion {
        self.numbers(a, b, |x, y| number::numeric(op, x, y))
    }
    fn rt_compare(&mut self, op: CompareOp, a: Value, b: Value) -> Completion {
        self.predicate(a, b, |x, y| number::compare(op, x, y))
    }
    fn rt_unary(&mut self, op: UnaryOp, a: Value) -> Completion {
        self.slow_calls += 1;
        match Self::to_number(a) {
            Some(x) => Ok(Value::number(number::unary(op, x))),
            None => self.fail(),
        }
    }
    fn rt_loose_equal(&mut self, a: Value, b: Value) -> Completion {
        self.slow_calls += 1;
        // IsLooselyEqual on primitives the toy has: null and undefined
        // equal each other only; booleans compare as numbers.
        if a.is_nullish() || b.is_nullish() {
            return Ok(Value::from_bool(a.is_nullish() && b.is_nullish()));
        }
        match (Self::to_number(a), Self::to_number(b)) {
            (Some(x), Some(y)) => Ok(Value::from_bool(x == y)),
            _ => self.fail(),
        }
    }
    fn rt_strict_equal(&mut self, a: Value, b: Value) -> bool {
        self.slow_calls += 1;
        // No strings or objects in the toy: identity is bit equality.
        a == b
    }
    fn rt_to_boolean(&mut self, v: Value) -> bool {
        self.slow_calls += 1;
        match v.kind() {
            Kind::Undefined | Kind::Null => false,
            Kind::Bool(b) => b,
            Kind::Int32(i) => i != 0,
            Kind::Double(d) => !(d == 0.0 || d.is_nan()),
            Kind::Object(_) | Kind::String(_) | Kind::Other(..) | Kind::Uninitialized => true,
        }
    }
}

/// How a [`run`] ended.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Outcome {
    Returned(Value),
    Threw(Value),
    /// `max_steps` instructions ran without finishing.
    StepLimit,
}

/// Runs verified bytecode from offset 0 for at most `max_steps`
/// instructions, with the context's registers and constants as they are.
/// Calls throw (there is no program to call into; see [`run_program`]).
///
/// # Panics
/// If the code doesn't decode or a jump leaves it: the verifier rules
/// those out, which is what the `bytecode_verify` fuzz target checks.
pub fn run(code: &[u8], cx: &mut ToyCtx, max_steps: u64) -> Outcome {
    run_with_handlers(code, &[], cx, max_steps)
}

/// [`run`] with an exception table: a throw (other than a termination)
/// at a pc covered by a handler stores the exception in the handler's
/// register and continues at its target.
///
/// # Panics
/// As [`run`]; the handlers must have passed
/// [`verify_handlers`](crate::verify_handlers).
pub fn run_with_handlers(
    code: &[u8],
    handlers: &[Handler],
    cx: &mut ToyCtx,
    max_steps: u64,
) -> Outcome {
    let mut pc = 0usize;
    for _ in 0..max_steps {
        let (insn, len) = decode(code, pc).expect("verified bytecode decodes");
        let mut flow = execute(cx, insn);
        if let Flow::Call = flow {
            flow = cx.rt_throw_error(ErrorKind::NotCallable);
        }
        match flow {
            Flow::Next => pc += len,
            Flow::Jump(l) => pc = jump_target(pc, l.0),
            Flow::Return(v) => return Outcome::Returned(v),
            Flow::Throw => {
                let exception = cx.exception.take().expect("a throw sets the exception");
                match find_handler(handlers, pc) {
                    Some(h) if !cx.terminating => {
                        cx.set(h.exception, exception);
                        pc = h.target as usize;
                    }
                    _ => return Outcome::Threw(exception),
                }
            }
            Flow::Call => unreachable!("turned into a throw above"),
        }
    }
    Outcome::StepLimit
}

fn jump_target(pc: usize, offset: i32) -> usize {
    usize::try_from(pc as i64 + offset as i64).expect("verified jump target")
}

fn find_handler(handlers: &[Handler], pc: usize) -> Option<&Handler> {
    handlers
        .iter()
        .find(|h| (h.start as usize..h.end as usize).contains(&pc))
}

/// A caller's frame, saved while its callee runs.
struct Saved {
    func: usize,
    /// The pc of the call instruction (for unwinding) and after it.
    call_pc: usize,
    resume_pc: usize,
    dst: Reg,
    regs: Vec<Value>,
    feedback: Vec<Feedback>,
    env: Option<usize>,
    closure: Option<Value>,
}

/// Runs `program[entry]` as a function called with `args`, for at most
/// `max_steps` instructions, following calls through a frame stack.
/// Every function must have passed [`verify`](crate::verify) and
/// [`verify_handlers`](crate::verify_handlers) against its limits, and its
/// nested-function table must index into `program`.
///
/// Calls copy up to `params` arguments into the callee's first registers
/// (the rest of its registers start as `undefined`), check for interrupts
/// at entry (D-007), and throw a `RangeError` past [`ToyCtx::max_depth`]
/// frames. A tail call replaces the caller's frame. A throw unwinds
/// through handlers and then callers; a termination unwinds everything.
///
/// # Panics
/// If the program breaks the rules above.
pub fn run_program(
    program: &[ToyFunction],
    entry: usize,
    args: &[Value],
    cx: &mut ToyCtx,
    max_steps: u64,
) -> Outcome {
    let max_depth = if cx.max_depth == 0 {
        10_000
    } else {
        cx.max_depth
    };
    let mut frames: Vec<Saved> = Vec::new();
    cx.closures.push(ToyClosure {
        func: entry,
        env: None,
    });
    let closure = Value::object(HeapRef(CLOSURE_BIT | (cx.closures.len() as u32 - 1)));
    let mut func = entry;
    enter(cx, program, entry, closure, None, args);
    let mut pc = 0usize;
    for _ in 0..max_steps {
        let f = &program[func];
        let (insn, len) = decode(&f.code, pc).expect("verified bytecode decodes");
        let mut flow = execute(cx, insn);
        if let Flow::Call = flow {
            let site = CallSite::of(&insn).expect("only calls return Flow::Call");
            flow = call(
                cx,
                program,
                &mut frames,
                &mut func,
                &mut pc,
                len,
                site,
                max_depth,
            );
            if flow == Flow::Next {
                // `call` switched to the callee at pc 0.
                continue;
            }
        }
        match flow {
            Flow::Next => pc += len,
            Flow::Jump(l) => pc = jump_target(pc, l.0),
            Flow::Return(v) => match frames.pop() {
                None => return Outcome::Returned(v),
                Some(saved) => {
                    pc = saved.resume_pc;
                    let dst = saved.dst;
                    func = restore(cx, program, saved);
                    cx.set(dst, v);
                }
            },
            Flow::Throw => {
                let exception = cx.exception.take().expect("a throw sets the exception");
                loop {
                    let handler = find_handler(&program[func].handlers, pc).copied();
                    match handler {
                        Some(h) if !cx.terminating => {
                            cx.set(h.exception, exception);
                            pc = h.target as usize;
                            break;
                        }
                        _ => match frames.pop() {
                            Some(saved) if !cx.terminating => {
                                pc = saved.call_pc;
                                func = restore(cx, program, saved);
                            }
                            _ => return Outcome::Threw(exception),
                        },
                    }
                }
            }
            Flow::Call => unreachable!("handled above"),
        }
    }
    Outcome::StepLimit
}

/// Starts running `program[func]` as `closure` in the current context.
fn enter(
    cx: &mut ToyCtx,
    program: &[ToyFunction],
    func: usize,
    closure: Value,
    env: Option<usize>,
    args: &[Value],
) {
    let f = &program[func];
    cx.regs = vec![Value::UNDEFINED; f.limits.registers as usize];
    for (r, &a) in cx.regs.iter_mut().zip(args).take(f.params as usize) {
        *r = a;
    }
    cx.constants = f.constants.clone();
    cx.feedback = vec![Feedback::NONE; f.limits.ics as usize];
    cx.functions = f.functions.clone();
    cx.env = env;
    cx.closure = Some(closure);
}

fn restore(cx: &mut ToyCtx, program: &[ToyFunction], saved: Saved) -> usize {
    let f = &program[saved.func];
    cx.regs = saved.regs;
    cx.feedback = saved.feedback;
    cx.constants = f.constants.clone();
    cx.functions = f.functions.clone();
    cx.env = saved.env;
    cx.closure = saved.closure;
    saved.func
}

/// Performs a call: on success switches the context to the callee at
/// pc 0 and returns `Flow::Next`; otherwise returns `Flow::Throw`.
#[allow(clippy::too_many_arguments)]
fn call(
    cx: &mut ToyCtx,
    program: &[ToyFunction],
    frames: &mut Vec<Saved>,
    func: &mut usize,
    pc: &mut usize,
    len: usize,
    site: CallSite,
    max_depth: usize,
) -> Flow {
    let callee = cx.reg(site.callee);
    let Some(closure) = cx.closure_of(callee) else {
        return cx.rt_throw_error(ErrorKind::NotCallable);
    };
    if !site.tail && frames.len() >= max_depth {
        return cx.rt_throw_error(ErrorKind::StackOverflow);
    }
    if let Flow::Throw = cx.check_interrupt() {
        return Flow::Throw;
    }
    let args: Vec<Value> = (0..site.argc.0)
        .map(|i| cx.reg(Reg(site.args.0 + i)))
        .collect();
    if !site.tail {
        frames.push(Saved {
            func: *func,
            call_pc: *pc,
            resume_pc: *pc + len,
            dst: site.dst,
            regs: std::mem::take(&mut cx.regs),
            feedback: std::mem::take(&mut cx.feedback),
            env: cx.env,
            closure: cx.closure,
        });
    }
    enter(cx, program, closure.func, callee, closure.env, &args);
    *func = closure.func;
    *pc = 0;
    Flow::Next
}
