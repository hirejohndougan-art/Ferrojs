//! FerroJS embedding API (task 1.9, D-054): run JavaScript from Rust.
//!
//! An [`Engine`] owns one heap, one global environment and the host
//! functions installed on it. Scripts run in order share the globals, like
//! several `<script>` elements on a page. Values cross the boundary as
//! owned [`JsValue`]s; nothing an embedder holds points into the heap, so
//! the collector is free to run (and later to move cells) at any time.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use ferrojs::{Engine, JsValue};
//!
//! let mut engine = Engine::new().unwrap();
//! let out = Rc::new(RefCell::new(Vec::new()));
//! let sink = out.clone();
//! engine
//!     .register("print", move |args| {
//!         sink.borrow_mut().push(args.first().map(|a| a.to_string()).unwrap_or_default());
//!         Ok(JsValue::Undefined)
//!     })
//!     .unwrap();
//! engine.run("let x = 6 * 7; print('x = ' + x);").unwrap();
//! assert_eq!(*out.borrow(), ["x = 42"]);
//! assert_eq!(engine.global("x"), Some(JsValue::Number(42.0)));
//!
//! let err = engine.run("y + 1").unwrap_err();
//! assert_eq!(err.name(), Some("ReferenceError"));
//! ```
//!
//! This is the minimal Phase 1 API. Embedding API 1.0 (derive macros,
//! Promise and async bridges, code caching) is Phase 4.

use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ferrojs_bytecode::ErrorKind;
use ferrojs_compiler::CompileError;
use ferrojs_vm::interp::{LoadError, Outcome};
use ferrojs_vm::{Vm, VmOptions};

/// A JavaScript value outside the engine: primitives by value, objects and
/// functions as markers (no handles to them yet).
pub use ferrojs_vm::HostValue as JsValue;

/// Engine settings.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Heap size in bytes (rounded up to the heap's 256 KiB chunks).
    pub heap_size: u64,
    /// Frame stack size in 8-byte slots; deeper recursion is a
    /// `RangeError`.
    pub stack_slots: u32,
    /// Collect garbage before every allocation (for testing embedders;
    /// very slow).
    pub gc_stress: bool,
}

impl Default for Options {
    fn default() -> Options {
        let vm = VmOptions::default();
        Options {
            heap_size: vm.heap_size,
            stack_slots: vm.stack_slots,
            gc_stress: vm.cx.gc_stress,
        }
    }
}

/// Why running a script failed.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The source has a syntax or early error (nothing ran).
    Syntax {
        message: String,
        /// 1-based line and column (in UTF-16 code units, like browsers),
        /// if known.
        position: Option<(u32, u32)>,
    },
    /// Valid JavaScript that FerroJS can't compile or run yet. Nothing ran
    /// if it was found while compiling.
    Unsupported(String),
    /// The script threw and nothing caught it.
    Uncaught {
        /// The thrown value. Until error objects exist (task 1.13), errors
        /// the engine throws are `undefined` with `name` and `message` set.
        value: JsValue,
        /// The error constructor's name, for errors the engine threw.
        name: Option<&'static str>,
        /// What went wrong, for errors the engine threw.
        message: Option<&'static str>,
    },
    /// [`Engine::interrupt_handle`] was set while the script ran.
    Terminated,
    /// The heap is full.
    OutOfMemory,
}

impl Error {
    /// The JavaScript error constructor this corresponds to, if any:
    /// `SyntaxError` for syntax errors, the engine's error kind for
    /// uncaught errors it threw.
    pub fn name(&self) -> Option<&'static str> {
        match self {
            Error::Syntax { .. } => Some("SyntaxError"),
            Error::Uncaught { name, .. } => *name,
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax { message, position } => {
                write!(f, "SyntaxError: {message}")?;
                if let Some((line, col)) = position {
                    write!(f, " (line {line}, column {col})")?;
                }
                Ok(())
            }
            Error::Unsupported(what) => write!(f, "not supported by FerroJS yet: {what}"),
            Error::Uncaught {
                name: Some(name),
                message,
                ..
            } => write!(f, "Uncaught {name}: {}", message.unwrap_or("")),
            Error::Uncaught { value, .. } => write!(f, "Uncaught {value}"),
            Error::Terminated => f.write_str("terminated"),
            Error::OutOfMemory => f.write_str("out of memory"),
        }
    }
}

impl std::error::Error for Error {}

/// Why an engine couldn't be made or a host function installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupError(String);

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SetupError {}

/// One JavaScript engine instance. Not `Send`: use one per thread.
pub struct Engine {
    vm: Vm,
}

impl Engine {
    /// An engine with the default [`Options`].
    pub fn new() -> Result<Engine, SetupError> {
        Engine::with_options(Options::default())
    }

    pub fn with_options(options: Options) -> Result<Engine, SetupError> {
        let mut vm_options = VmOptions {
            heap_size: options.heap_size,
            stack_slots: options.stack_slots,
            ..VmOptions::default()
        };
        vm_options.cx.gc_stress = options.gc_stress;
        let vm = Vm::new(vm_options).map_err(|e| SetupError(format!("heap: {e:?}")))?;
        Ok(Engine { vm })
    }

    /// Installs `f` as global function `name` (a writable, configurable,
    /// non-enumerable property of the global object, like a built-in).
    /// `f` gets the arguments; `Ok` is its result and `Err` is thrown.
    /// Both must be primitives: returning an object or a function throws
    /// in the script instead. Host functions can't call back into
    /// JavaScript yet. A host function that panics leaves the engine in
    /// an unknown state: drop it rather than catching the panic and
    /// running more code.
    pub fn register(
        &mut self,
        name: &str,
        mut f: impl FnMut(&[JsValue]) -> Result<JsValue, JsValue> + 'static,
    ) -> Result<(), SetupError> {
        self.vm
            .define_native(name, Box::new(move |_this, args| f(args)))
            .map_err(|e| SetupError(format!("can't define {name}: {e}")))
    }

    /// Compiles and runs `source` as a classic (non-module) script.
    pub fn run(&mut self, source: &str) -> Result<(), Error> {
        let f = ferrojs_compiler::compile_script_source(source).map_err(|e| match e {
            CompileError::Syntax(errors) => {
                let first = errors.first();
                Error::Syntax {
                    message: first.map_or_else(|| "invalid script".into(), |e| e.message.clone()),
                    position: first.and_then(|e| e.offset).map(|o| line_col(source, o)),
                }
            }
            CompileError::Unsupported { what, offset } => {
                let (line, col) = line_col(source, offset);
                Error::Unsupported(format!("{what} (line {line}, column {col})"))
            }
            CompileError::TooLarge(what) => {
                Error::Unsupported(format!("function too large: {what}"))
            }
        })?;
        let outcome = self.vm.run_script(&f).map_err(|e| match e {
            LoadError::OutOfMemory => Error::OutOfMemory,
            LoadError::Verify { function, error } => {
                // The compiler made bytecode its own verifier rejects.
                Error::Unsupported(format!("internal error: {function}: {error:?}"))
            }
        })?;
        match outcome {
            Outcome::Returned(_) => Ok(()),
            Outcome::Threw(v, kind) => {
                let value = self.vm.host_value(v);
                Err(match kind {
                    Some(k) => {
                        let (name, message) = describe(k);
                        Error::Uncaught {
                            value,
                            name: Some(name),
                            message: Some(message),
                        }
                    }
                    None => Error::Uncaught {
                        value,
                        name: None,
                        message: None,
                    },
                })
            }
            Outcome::Terminated => Err(Error::Terminated),
            Outcome::OutOfMemory => Err(Error::OutOfMemory),
        }
    }

    /// Global `name`'s value: a `let` or `const` binding, else a property
    /// of the global object. `None` if there is neither, or the binding is
    /// still uninitialized.
    pub fn global(&self, name: &str) -> Option<JsValue> {
        let v = self.vm.global(name)?;
        if v.is_uninitialized() {
            return None;
        }
        Some(self.vm.host_value(v))
    }

    /// A flag that stops running code when set, from any thread: the run
    /// fails with [`Error::Terminated`]. It stays set until cleared, and
    /// while set every run stops at its first check.
    pub fn interrupt_handle(&self) -> Arc<AtomicBool> {
        self.vm.interrupt.clone()
    }
}

/// The error constructor and a message for an error the engine threw.
fn describe(k: ErrorKind) -> (&'static str, &'static str) {
    match k {
        ErrorKind::UninitializedBinding => (
            "ReferenceError",
            "cannot access a variable before its declaration has run",
        ),
        ErrorKind::ConstAssignment => ("TypeError", "assignment to a constant"),
        ErrorKind::NotCallable => ("TypeError", "not a function"),
        ErrorKind::StackOverflow => ("RangeError", "maximum call stack size exceeded"),
        ErrorKind::UnresolvableReference => ("ReferenceError", "a name is not defined"),
        ErrorKind::GlobalRedeclaration => (
            "SyntaxError",
            "a global declaration clashes with an existing one",
        ),
        ErrorKind::InvalidAssignmentTarget => ("ReferenceError", "invalid assignment target"),
        ErrorKind::NullishBase => (
            "TypeError",
            "cannot read or write properties of undefined or null",
        ),
        ErrorKind::NotAnObject => ("TypeError", "the right side of 'in' is not an object"),
        ErrorKind::PropertyRefused => ("TypeError", "the property can't be assigned or deleted"),
        ErrorKind::HostResult => (
            "TypeError",
            "a host function returned an object or a function",
        ),
        ErrorKind::NotAConstructor => ("TypeError", "not a constructor"),
        ErrorKind::BadInstanceOf => (
            "TypeError",
            "the right side of 'instanceof' is not callable or has no object prototype",
        ),
    }
}

/// The 1-based line and column (UTF-16 units) of byte `offset` in `source`.
fn line_col(source: &str, offset: u32) -> (u32, u32) {
    let end = (offset as usize).min(source.len());
    // Back up to a character boundary.
    let end = (0..=end)
        .rev()
        .find(|&i| source.is_char_boundary(i))
        .unwrap_or(0);
    let (mut line, mut col) = (1u32, 1u32);
    for (i, c) in source[..end].char_indices() {
        match c {
            // CRLF is one line terminator: its `\n` ends the line.
            '\r' if source[i + 1..].starts_with('\n') => {}
            '\r' | '\n' | '\u{2028}' | '\u{2029}' => (line, col) = (line + 1, 1),
            c => col += c.len_utf16() as u32,
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::line_col;

    #[test]
    fn line_col_counts_terminators_and_utf16_units() {
        assert_eq!(line_col("abc", 0), (1, 1));
        assert_eq!(line_col("abc", 2), (1, 3));
        assert_eq!(line_col("a\nbc", 3), (2, 2));
        assert_eq!(line_col("a\r\nbc", 4), (2, 2));
        // The `\n` of a CRLF still belongs to the first line.
        assert_eq!(line_col("a\r\nb", 2), (1, 2));
        assert_eq!(line_col("a\r\nb", 3), (2, 1));
        assert_eq!(line_col("a\rb\u{2028}c", 6), (3, 1));
        // An astral character is two UTF-16 units.
        assert_eq!(line_col("😀x", 4), (1, 3));
        // Past the end, or inside a character: clamped.
        assert_eq!(line_col("ab", 99), (1, 3));
        assert_eq!(line_col("😀", 2), (1, 1));
    }
}
