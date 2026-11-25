//! Host functions (task 1.9, D-054): Rust closures that JavaScript can
//! call, like the `print` the command-line tool and the test runners
//! install.
//!
//! A host function never sees the heap. The interpreter copies `this` and
//! the arguments into owned [`HostValue`]s before the call and turns the
//! result back into a value after it, so nothing a host function does can
//! leave a dangling reference or skip a root. The price is a copy of every
//! string argument, which is fine for host hooks; the built-ins (1.13) are
//! written against the heap directly and don't go through this.
//!
//! A host function is a `Native` cell holding an index into the
//! [`Vm`](crate::Vm)'s table of closures; `typeof` says `"function"`.
//! Host functions can't call back into JavaScript yet.

use std::fmt;

use ferrojs_bytecode::number;

/// A JavaScript value copied out of the heap, or one a host function
/// returns.
#[derive(Clone, Debug, PartialEq)]
pub enum HostValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    /// A string, with lone surrogates replaced by U+FFFD.
    String(String),
    /// An object (no identity: host functions can't hold on to objects
    /// yet). Can't be returned.
    Object,
    /// A function. Can't be returned.
    Function,
}

impl HostValue {
    /// Whether a host function may return or throw this (objects and
    /// functions can't be made from the host side yet).
    pub fn is_primitive(&self) -> bool {
        !matches!(self, HostValue::Object | HostValue::Function)
    }
}

impl From<f64> for HostValue {
    fn from(x: f64) -> HostValue {
        HostValue::Number(x)
    }
}

impl From<bool> for HostValue {
    fn from(b: bool) -> HostValue {
        HostValue::Bool(b)
    }
}

impl From<&str> for HostValue {
    fn from(s: &str) -> HostValue {
        HostValue::String(s.to_owned())
    }
}

impl From<String> for HostValue {
    fn from(s: String) -> HostValue {
        HostValue::String(s)
    }
}

/// `String(v)` (ECMA-262 ToString) for primitives. Objects print as
/// `[object Object]` (what `Object.prototype.toString` will give) and
/// functions as a placeholder until `Function.prototype.toString` exists.
impl fmt::Display for HostValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostValue::Undefined => f.write_str("undefined"),
            HostValue::Null => f.write_str("null"),
            HostValue::Bool(b) => write!(f, "{b}"),
            HostValue::Number(x) => f.write_str(&number::to_js_string(*x)),
            HostValue::String(s) => f.write_str(s),
            HostValue::Object => f.write_str("[object Object]"),
            HostValue::Function => f.write_str("function () { [native code] }"),
        }
    }
}

/// A host function: called with `this` and the arguments; `Ok` is the
/// result, `Err` is thrown. Both must be primitives
/// ([`HostValue::is_primitive`]); returning an object or a function throws
/// instead.
pub type NativeFn = Box<dyn FnMut(&HostValue, &[HostValue]) -> Result<HostValue, HostValue>>;

/// Why [`Vm::define_native`](crate::Vm::define_native) failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefineError {
    /// The global object refused the property (a script declared that
    /// name with `var` or `function`, which is non-configurable).
    Refused,
    /// The heap is full.
    OutOfMemory,
}

impl fmt::Display for DefineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DefineError::Refused => f.write_str("the global object refused the property"),
            DefineError::OutOfMemory => f.write_str("out of memory"),
        }
    }
}

impl std::error::Error for DefineError {}
