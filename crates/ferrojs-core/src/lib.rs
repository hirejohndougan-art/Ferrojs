//! The audited unsafe core.
//!
//! This crate holds the only `unsafe` code in the engine (D-004): the 4 GB
//! heap cage, and later GC internals, JIT executable memory and tier
//! trampolines. Every `unsafe` block must carry a `// SAFETY:` comment
//! naming the invariant it relies on, and comes with tests that exercise
//! it so Miri (D-016) has something to check.
//!
//! - [`cage`]: the address-space reservation, chunk commits and checked
//!   accessors (D-002, D-022, D-027).
//! - [`heap`]: cell allocation in size-class chunks and the mark-sweep
//!   collector.
//! - [`gc`]: the engine context, lifetime-branded handles, roots and the
//!   write barrier on top of the collector (D-023, D-030).

#![deny(clippy::undocumented_unsafe_blocks)]

pub mod cage;
pub mod gc;
pub mod heap;

pub use cage::{Cage, CageError};
pub use gc::{Cx, CxOptions, Gc, Global, NoGc, Root, Scope, Unbound};
pub use heap::{AllocError, CollectStats, Heap, HeapOptions, Trace};

#[cfg(test)]
mod tests {
    /// Exists only under Miri. `tools/miri.sh` looks for it in the output,
    /// so a run that silently didn't use Miri can't pass as one that did.
    #[cfg(miri)]
    #[test]
    fn running_under_miri() {}
}
