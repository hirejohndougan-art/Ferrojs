//! A reference context and dispatch loop over plain values, for tests,
//! benchmarks and fuzzing of the op definitions before the real
//! interpreter (task 1.7) exists. Primitives only: the slow paths convert
//! `undefined`, `null`, booleans and numbers as ToNumber does, and throw
//! `undefined` for heap values (strings, objects, symbols), which the toy
//! can't look inside.

use ferrojs_value::{Kind, Value};

use crate::number::{self, CompareOp, NumericOp, UnaryOp};
use crate::Handler;
use crate::{
    decode, execute, Completion, Const, EnvSlot, ErrorKind, Feedback, Flow, Ic, Limits, OpCtx, Reg,
    Thrown,
};

/// Registers, constants, feedback and an environment chain in vectors.
#[derive(Debug, Default)]
pub struct ToyCtx {
    pub regs: Vec<Value>,
    pub constants: Vec<Value>,
    pub feedback: Vec<Feedback>,
    /// Innermost environment last; `depth` 0 is the innermost.
    pub envs: Vec<Vec<Value>>,
    pub exception: Option<Value>,
    /// The interrupt check terminates once this many checks have passed.
    pub interrupt_after: Option<u64>,
    pub interrupt_checks: u64,
    /// How many times each slow path ran (for tests of the fast paths).
    pub slow_calls: u64,
    /// Set when the interrupt check terminated execution: termination is
    /// not catchable, so [`run_with_handlers`] doesn't unwind into
    /// handlers then.
    pub terminating: bool,
    /// The kind of the last error a runtime function threw, if any (the
    /// toy has no error objects; the exception value is `undefined`).
    pub last_error: Option<ErrorKind>,
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

    fn env_index(&self, slot: EnvSlot) -> Option<(usize, usize)> {
        let depth = slot.depth as usize;
        let e = self.envs.len().checked_sub(depth + 1)?;
        let s = slot.slot as usize;
        (s < self.envs[e].len()).then_some((e, s))
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
            Some((e, s)) => Ok(self.envs[e][s]),
            None => {
                self.exception = Some(Value::UNDEFINED);
                Err(Thrown)
            }
        }
    }
    fn env_set(&mut self, slot: EnvSlot, v: Value) -> Result<(), Thrown> {
        match self.env_index(slot) {
            Some((e, s)) => {
                self.envs[e][s] = v;
                Ok(())
            }
            None => {
                self.exception = Some(Value::UNDEFINED);
                Err(Thrown)
            }
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
/// instructions.
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
        match execute(cx, insn) {
            Flow::Next => pc += len,
            Flow::Jump(l) => {
                pc = usize::try_from(pc as i64 + l.0 as i64).expect("verified jump target");
            }
            Flow::Return(v) => return Outcome::Returned(v),
            Flow::Throw => {
                let exception = cx.exception.take().expect("a throw sets the exception");
                let handler = handlers
                    .iter()
                    .find(|h| (h.start as usize..h.end as usize).contains(&pc));
                match handler {
                    Some(h) if !cx.terminating => {
                        cx.set(h.exception, exception);
                        pc = h.target as usize;
                    }
                    _ => return Outcome::Threw(exception),
                }
            }
        }
    }
    Outcome::StepLimit
}
