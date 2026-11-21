//! A reference context and dispatch loop over plain values, for tests,
//! benchmarks and fuzzing of the op definitions before the real
//! interpreter (task 1.7) exists. Numbers only: the slow paths handle
//! numbers and throw `undefined` for anything else.

use ferrojs_value::{Kind, Value};

use crate::{
    decode, execute, Completion, Const, EnvSlot, Feedback, Flow, Ic, Limits, OpCtx, Reg, Thrown,
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

    fn numbers(&mut self, a: Value, b: Value, f: impl FnOnce(f64, f64) -> f64) -> Completion {
        self.slow_calls += 1;
        match (a.as_number(), b.as_number()) {
            (Some(x), Some(y)) => Ok(Value::number(f(x, y))),
            _ => {
                self.exception = Some(Value::UNDEFINED);
                Err(Thrown)
            }
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
    fn rt_less_than(&mut self, a: Value, b: Value) -> Completion {
        self.slow_calls += 1;
        match (a.as_number(), b.as_number()) {
            (Some(x), Some(y)) => Ok(Value::from_bool(x < y)),
            _ => {
                self.exception = Some(Value::UNDEFINED);
                Err(Thrown)
            }
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
            Kind::Object(_) | Kind::String(_) | Kind::Other(..) => true,
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
                return Outcome::Threw(cx.exception.take().expect("a throw sets the exception"));
            }
        }
    }
    Outcome::StepLimit
}
