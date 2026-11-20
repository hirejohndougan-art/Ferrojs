//! Heap allocation micro-benchmarks (D-022, D-027): the bump allocator in
//! the cage, which every object allocation will go through.
//!
//! Linux-only like the value benchmarks: every item is behind
//! `cfg(target_os = "linux")`, and other platforms get an empty `main`.

#[cfg(target_os = "linux")]
use std::hint::black_box;

#[cfg(target_os = "linux")]
use ferrojs_core::Heap;
#[cfg(target_os = "linux")]
use gungraun::prelude::*;

/// Cells allocated per benchmark run.
#[cfg(target_os = "linux")]
const CELLS: u32 = 1024;

/// A fresh heap with its first chunk already committed, so the measured
/// part is the bump path rather than the first `mprotect`.
#[cfg(target_os = "linux")]
fn warm_heap() -> Heap {
    let mut heap = Heap::new(64 * ferrojs_core::cage::CHUNK_SIZE as u64).expect("reserve heap");
    heap.alloc(8).expect("first cell");
    heap
}

// Allocates 1024 small cells of mixed sizes and writes each one's header
// word, as object creation will.
#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::mixed(setup = warm_heap)]
fn bump_alloc(mut heap: Heap) -> u64 {
    let mut acc = 0u64;
    for i in 0..CELLS {
        let size = 16 + (i % 4) * 8;
        let at = heap.alloc(black_box(size)).expect("alloc");
        heap.write_u64(at, i as u64);
        acc = acc.wrapping_add(at as u64);
    }
    acc
}

#[cfg(target_os = "linux")]
library_benchmark_group!(name = heap_alloc, benchmarks = [bump_alloc]);

#[cfg(target_os = "linux")]
main!(library_benchmark_groups = heap_alloc);

#[cfg(not(target_os = "linux"))]
fn main() {}
