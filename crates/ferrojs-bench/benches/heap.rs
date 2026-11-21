//! Heap allocation micro-benchmarks (D-022, D-027): the bump allocator in
//! the cage, which every object allocation will go through.
//!
//! Linux-only like the value benchmarks: every item is behind
//! `cfg(target_os = "linux")`, and other platforms get an empty `main`.

#[cfg(target_os = "linux")]
use std::hint::black_box;

#[cfg(target_os = "linux")]
use ferrojs_core::gc::{Cx, CxOptions, NoEdges};
#[cfg(target_os = "linux")]
use ferrojs_core::{Cage, CollectStats, Heap, HeapOptions, Trace};
#[cfg(target_os = "linux")]
use gungraun::prelude::*;

/// Cells allocated per benchmark run.
#[cfg(target_os = "linux")]
const CELLS: u32 = 1024;

/// A fresh heap with a chunk already committed for each size class used,
/// so the measured part is the allocation path rather than `mprotect`.
#[cfg(target_os = "linux")]
fn warm_heap() -> Heap {
    let mut heap = Heap::new(64 * ferrojs_core::cage::CHUNK_SIZE as u64).expect("reserve heap");
    // One cell per size class the benchmark uses (16, 24, 32, 48).
    for size in [16, 24, 32, 40] {
        heap.alloc(size).expect("first cell");
    }
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

/// Test cells: a header word, then one reference in the next word.
#[cfg(target_os = "linux")]
struct ListTracer;

#[cfg(target_os = "linux")]
impl Trace for ListTracer {
    fn trace(&self, cage: &Cage, cell: u32, _size: u32, edges: &mut Vec<u32>) {
        edges.push(cage.read_u32(cell + 8));
    }
}

/// A heap holding two linked lists of 1024 16-byte cells each, with
/// poisoning and verification off (release defaults). Returns the heap
/// and the head of the list that stays reachable.
#[cfg(target_os = "linux")]
fn two_lists() -> (Heap, u32) {
    let cage = Cage::new(64 * ferrojs_core::cage::CHUNK_SIZE as u64).expect("reserve cage");
    let mut heap = Heap::from_cage(
        cage,
        HeapOptions {
            poison: false,
            verify: false,
        },
    );
    let mut heads = [0u32; 2];
    for i in 0..2 * CELLS {
        let at = heap.alloc(16).expect("alloc");
        let list = (i % 2) as usize;
        heap.write_u64(at, 1);
        heap.write_u32(at + 8, heads[list]);
        heads[list] = at;
    }
    (heap, heads[0])
}

// Marks a 1024-cell list and sweeps another 1024 dead cells.
#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::half_live(setup = two_lists)]
fn collect((mut heap, root): (Heap, u32)) -> CollectStats {
    heap.collect([black_box(root)], &ListTracer)
}

/// A context over a warm heap (as in `bump_alloc`), stress mode off.
#[cfg(target_os = "linux")]
fn warm_cx() -> Cx {
    Cx::new(
        warm_heap(),
        Box::new(NoEdges),
        CxOptions { gc_stress: false },
    )
}

// The same allocations through the engine context's branded handles
// (D-023, D-030): allocate, bind, write the header through the token, and
// root every eighth cell in a scope.
#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::mixed(setup = warm_cx)]
fn cx_alloc(mut cx: Cx) -> u64 {
    cx.scope(|s| {
        let mut acc = 0u64;
        for i in 0..CELLS {
            let size = 16 + (i % 4) * 8;
            let cell = s.alloc::<()>(black_box(size)).expect("alloc");
            let cell = cell.bind(s.nogc());
            s.nogc().write_u64(cell, 0, i as u64);
            if i % 8 == 0 {
                s.root(cell);
            }
            acc = acc.wrapping_add(cell.offset() as u64);
        }
        acc
    })
}

#[cfg(target_os = "linux")]
library_benchmark_group!(
    name = heap_alloc,
    benchmarks = [bump_alloc, collect, cx_alloc]
);

#[cfg(target_os = "linux")]
main!(library_benchmark_groups = heap_alloc);

#[cfg(not(target_os = "linux"))]
fn main() {}
