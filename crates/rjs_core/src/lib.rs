//! The audited unsafe core.
//!
//! This crate will hold the only `unsafe` code in the engine: the 4 GB heap
//! cage, GC internals, JIT executable memory and tier trampolines. Every
//! `unsafe` block must carry a `// SAFETY:` comment naming the invariant it
//! relies on. Nothing lives here yet; it is created now so the boundary
//! exists before any code is written.
