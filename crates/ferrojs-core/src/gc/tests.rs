use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use super::*;
use crate::cage::CHUNK_SIZE;
use crate::heap::HeapOptions;

const C: u64 = CHUNK_SIZE as u64;

// The test cell layout: header word = kind 2 | id << 8 | edge count << 32,
// then one u32 reference per edge.
const TEST_KIND: u8 = 2;

struct Node;

struct TestTracer;

impl Trace for TestTracer {
    fn trace(&self, cage: &Cage, cell: u32, size: u32, edges: &mut Vec<u32>) {
        let header = cage.read_u64(cell);
        assert_eq!(header as u8, TEST_KIND, "traced a cell that isn't one");
        let len = (header >> 32) as u32;
        assert!(8 + 4 * len <= size);
        for i in 0..len {
            edges.push(cage.read_u32(cell + 8 + 4 * i));
        }
    }
}

/// A context with poisoning and the verifier on, over `chunks` chunks.
fn checked(chunks: u64, gc_stress: bool) -> Cx {
    let cage = Cage::new(chunks * C).expect("reserve a heap");
    let heap = Heap::from_cage(
        cage,
        HeapOptions {
            poison: true,
            verify: true,
        },
    );
    Cx::new(heap, Box::new(TestTracer), CxOptions { gc_stress })
}

/// Allocates a node with `id` and `edges` null edge slots.
fn node(cx: &mut Cx, id: u32, edges: u32) -> Unbound<Node> {
    let cell = cx.alloc::<Node>(8 + 4 * edges).unwrap();
    let nogc = cx.nogc();
    let cell = cell.bind(nogc);
    nogc.write_u64(
        cell,
        0,
        TEST_KIND as u64 | (id as u64) << 8 | (edges as u64) << 32,
    );
    cell.unbind()
}

fn panic_msg(err: Box<dyn std::any::Any + Send>) -> String {
    match err.downcast::<String>() {
        Ok(s) => *s,
        Err(err) => err.downcast_ref::<&str>().unwrap().to_string(),
    }
}

fn id_of(nogc: NoGc<'_>, cell: Gc<'_, Node>) -> u32 {
    (nogc.read_u64(cell, 0) >> 8) as u32 & 0xff_ffff
}

fn edge<'a>(nogc: NoGc<'a>, cell: Gc<'_, Node>, i: u32) -> Option<Gc<'a, Node>> {
    nogc.read_ref(cell, 8 + 4 * i)
}

fn set_edge(nogc: NoGc<'_>, cell: Gc<'_, Node>, i: u32, target: Option<Gc<'_, Node>>) {
    nogc.write_ref(cell, 8 + 4 * i, target)
}

#[test]
fn allocated_cells_read_back_what_was_written() {
    let mut cx = checked(8, false);
    let cell = cx.alloc::<()>(32).unwrap().bind(cx.nogc());
    let nogc = cx.nogc();
    assert!(nogc.is_live(cell));
    assert_eq!(nogc.cell_size(cell), 32);
    assert_eq!(nogc.read_u64(cell, 0), 0, "fresh cells are zeroed");
    nogc.write_u8(cell, 0, 7);
    nogc.write_u32(cell, 4, 0xdead_beef);
    nogc.write_u64(cell, 8, u64::MAX - 1);
    nogc.write_bytes(cell, 16, b"ferrojs!");
    nogc.write_value_bits(cell, 24, 0x7ff8_0000_0000_0001, None::<Gc<'_, ()>>);
    assert_eq!(nogc.read_u8(cell, 0), 7);
    assert_eq!(nogc.read_u32(cell, 4), 0xdead_beef);
    assert_eq!(nogc.read_u64(cell, 8), u64::MAX - 1);
    let mut buf = [0u8; 8];
    nogc.read_bytes(cell, 16, &mut buf);
    assert_eq!(&buf, b"ferrojs!");
    assert_eq!(nogc.read_u64(cell, 24), 0x7ff8_0000_0000_0001);
}

#[test]
fn references_round_trip() {
    let mut cx = checked(8, false);
    let a = node(&mut cx, 1, 2);
    let b = node(&mut cx, 2, 0);
    let nogc = cx.nogc();
    let (a, b) = (a.bind(nogc), b.bind(nogc));
    assert_eq!(edge(nogc, a, 0), None);
    set_edge(nogc, a, 1, Some(b));
    assert_eq!(edge(nogc, a, 1), Some(b));
    set_edge(nogc, a, 1, None);
    assert_eq!(edge(nogc, a, 1), None);
}

#[test]
fn unrooted_cells_are_collected_and_rooted_ones_kept() {
    let mut cx = checked(8, false);
    cx.scope(|s| {
        let keep = node(s, 10, 1).bind(s.nogc());
        let keep = s.root(keep);
        let child = node(s, 11, 0).bind(s.nogc());
        set_edge(s.nogc(), keep.get(s.nogc()), 0, Some(child));
        let garbage = node(s, 12, 0).bind(s.nogc()).offset();
        let stats = s.collect();
        assert_eq!(stats.freed_bytes, 16);
        let nogc = s.nogc();
        let keep = keep.get(nogc);
        assert_eq!(id_of(nogc, keep), 10);
        assert_eq!(id_of(nogc, edge(nogc, keep, 0).unwrap()), 11);
        assert!(!s.heap.is_live_cell(garbage));
    });
    assert_eq!(cx.handle_count(), 0);
    cx.collect();
    assert_eq!(cx.allocated_bytes(), 0, "leaving the scope unrooted it");
}

#[test]
fn nested_scopes_pop_only_their_own_roots() {
    let mut cx = checked(8, false);
    cx.scope(|outer| {
        let a = node(outer, 1, 0).bind(outer.nogc());
        let a = outer.root(a);
        let inner_cell = outer.scope(|inner| {
            let b = node(inner, 2, 0).bind(inner.nogc());
            let b = inner.root(b);
            assert_eq!(inner.handle_count(), 2);
            inner.collect();
            assert_eq!(id_of(inner.nogc(), b.get(inner.nogc())), 2);
            b.get(inner.nogc()).offset()
        });
        assert_eq!(outer.handle_count(), 1);
        outer.collect();
        assert!(!outer.heap.is_live_cell(inner_cell));
        assert_eq!(id_of(outer.nogc(), a.get(outer.nogc())), 1);
    });
}

#[test]
fn a_panicking_scope_still_pops_its_roots() {
    let mut cx = checked(8, false);
    let result = catch_unwind(AssertUnwindSafe(|| {
        cx.scope(|s| {
            let a = node(s, 1, 0).bind(s.nogc());
            s.root(a);
            panic!("boom");
        })
    }));
    assert!(result.is_err());
    assert_eq!(cx.handle_count(), 0);
    cx.collect();
    assert_eq!(cx.allocated_bytes(), 0);
}

#[test]
fn a_root_can_be_retargeted() {
    let mut cx = checked(8, false);
    cx.scope(|s| {
        let a = node(s, 1, 0).bind(s.nogc());
        let r = s.root(a);
        let b = node(s, 2, 0).bind(s.nogc());
        r.set(s.nogc(), b);
        s.collect();
        assert_eq!(id_of(s.nogc(), r.get(s.nogc())), 2);
        assert_eq!(s.allocated_bytes(), 16, "the first cell was freed");
    });
}

#[test]
fn globals_outlive_scopes_and_free_their_slot_when_dropped() {
    let mut cx = checked(8, false);
    let g = cx.scope(|s| {
        let a = node(s, 5, 0).bind(s.nogc());
        s.nogc().global(a)
    });
    assert_eq!(cx.global_count(), 1);
    cx.collect();
    assert_eq!(id_of(cx.nogc(), g.get(cx.nogc())), 5);
    let b = node(&mut cx, 6, 0).bind(cx.nogc());
    g.set(cx.nogc(), b);
    cx.collect();
    assert_eq!(cx.allocated_bytes(), 16);
    assert_eq!(id_of(cx.nogc(), g.get(cx.nogc())), 6);
    drop(g);
    assert_eq!(cx.global_count(), 0);
    cx.collect();
    assert_eq!(cx.allocated_bytes(), 0);
    // The freed slot is reused.
    let c = node(&mut cx, 7, 0).bind(cx.nogc());
    let g2 = cx.nogc().global(c);
    assert_eq!(g2.slot, 0);
    assert_eq!(cx.global_count(), 1);
}

#[test]
fn a_root_used_with_another_context_panics() {
    let mut cx1 = checked(8, false);
    let cx2 = checked(8, false);
    cx1.scope(|s| {
        let a = node(s, 1, 0).bind(s.nogc());
        let r = s.root(a);
        let err = catch_unwind(AssertUnwindSafe(|| r.get(cx2.nogc()))).unwrap_err();
        let msg = panic_msg(err);
        assert!(msg.contains("another context"), "{msg}");
    });
}

#[test]
fn a_global_used_with_another_context_panics() {
    let mut cx1 = checked(8, false);
    let cx2 = checked(8, false);
    let a = node(&mut cx1, 1, 0).bind(cx1.nogc());
    let g = cx1.nogc().global(a);
    let err = catch_unwind(AssertUnwindSafe(|| g.get(cx2.nogc()))).unwrap_err();
    let msg = panic_msg(err);
    assert!(msg.contains("another context"), "{msg}");
}

#[test]
fn a_global_can_outlive_its_context() {
    let mut cx = checked(8, false);
    let a = node(&mut cx, 1, 0).bind(cx.nogc());
    let g = cx.nogc().global(a);
    drop(cx);
    drop(g);
}

#[test]
fn stress_mode_collects_before_every_allocation() {
    let mut cx = checked(8, true);
    for i in 0..5 {
        let _ = node(&mut cx, i, 0);
    }
    assert_eq!(cx.collections(), 5);
    assert_eq!(cx.allocated_bytes(), 16, "only the newest cell is left");
}

#[test]
fn stress_mode_catches_an_unbound_kept_across_a_collection() {
    let mut cx = checked(8, true);
    let stale = node(&mut cx, 1, 0);
    // Collects first; `stale` was not rooted. (A cell of the same size
    // class could reuse the freed slot, and then binding can't tell.)
    let _ = node(&mut cx, 2, 8);
    let err = catch_unwind(AssertUnwindSafe(|| stale.bind(cx.nogc()))).unwrap_err();
    let msg = panic_msg(err);
    assert!(msg.contains("stale handle"), "{msg}");
}

#[test]
fn a_read_outside_the_cell_panics_with_the_verifier_on() {
    let mut cx = checked(8, false);
    let a = cx.alloc::<()>(16).unwrap().bind(cx.nogc());
    let nogc = cx.nogc();
    assert!(catch_unwind(AssertUnwindSafe(|| nogc.read_u64(a, 16))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| nogc.read_u32(a, 14))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| nogc.write_u64(a, 16, 0))).is_err());
    assert_eq!(nogc.read_u64(a, 8), 0);
}

#[test]
fn reading_a_dangling_reference_panics_with_the_verifier_on() {
    let mut cx = checked(8, false);
    let a = cx.alloc::<()>(16).unwrap().bind(cx.nogc());
    let nogc = cx.nogc();
    // A reference stored as plain data (a bug the barrier API exists to
    // prevent) that points into the middle of the cell.
    nogc.write_u32(a, 0, a.offset() + 8);
    assert!(catch_unwind(AssertUnwindSafe(|| nogc.read_ref::<(), ()>(a, 0))).is_err());
}

#[test]
fn a_full_heap_collects_and_retries() {
    // Chunks 1..=3 usable; each 4 KB cell takes a slot of a 4096-byte
    // class chunk, so a few dozen fill the heap.
    let mut cx = checked(4, false);
    let mut n = 0u32;
    loop {
        let fresh = cx.collections();
        let _ = cx.alloc::<()>(4096).unwrap();
        n += 1;
        if cx.collections() > fresh {
            break;
        }
        assert!(n < 1000, "never collected");
    }
    assert!(n > 10);
}

#[test]
fn allocation_errors_are_passed_on() {
    let mut cx = checked(4, false);
    assert!(matches!(cx.alloc::<()>(0), Err(AllocError::BadSize(0))));
    assert!(matches!(
        cx.alloc::<()>(16 * CHUNK_SIZE),
        Err(AllocError::BadSize(_) | AllocError::OutOfMemory { .. })
    ));
    let mut kept = Vec::new();
    let err = loop {
        match cx.alloc::<()>(4096) {
            Ok(u) => {
                let cell = u.bind(cx.nogc());
                cx.nogc().write_u64(cell, 0, TEST_KIND as u64);
                kept.push(cx.nogc().global(cell));
            }
            Err(e) => break e,
        }
    };
    assert!(matches!(err, AllocError::OutOfMemory { .. }), "{err:?}");
}

#[test]
fn collection_runs_once_enough_was_allocated() {
    let mut cx = checked(16, false);
    let mut allocated = 0u64;
    while cx.collections() == 0 {
        let _ = cx.alloc::<()>(1024).unwrap();
        allocated += 1024;
        assert!(allocated <= 2 * crate::heap::MIN_GC_THRESHOLD);
    }
    assert!(allocated > crate::heap::MIN_GC_THRESHOLD);
}

#[test]
fn options_come_from_the_environment_by_default() {
    // Only checks the plumbing; the variable is normally unset in tests.
    let expected = std::env::var_os("FERROJS_GC_STRESS").is_some_and(|v| v == "1");
    assert_eq!(CxOptions::default().gc_stress, expected);
}

/// A tiny xorshift generator, so the test needs no dependency.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as u32
    }
}

/// Builds and rewires a random graph in stress mode (every allocation
/// collects), keeping a Rust model of it, and checks after every step that
/// the heap graph reachable from the globals matches the model.
#[test]
fn stress_random_graph_matches_its_model() {
    let steps: u32 = if cfg!(miri) { 25 } else { 1500 };
    let mut cx = checked(if cfg!(miri) { 8 } else { 32 }, true);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    // id -> its two edges.
    let mut model: HashMap<u32, [Option<u32>; 2]> = HashMap::new();
    let mut globals: Vec<(Global<Node>, u32)> = Vec::new();
    for id in 1..=steps {
        match rng.below(10) {
            // Drop a global.
            0 | 1 if !globals.is_empty() => {
                let i = rng.below(globals.len() as u32) as usize;
                globals.swap_remove(i);
            }
            // Rewire an edge between reachable cells.
            2 if !globals.is_empty() => {
                let nogc = cx.nogc();
                let (g, gid) = &globals[rng.below(globals.len() as u32) as usize];
                let target = &globals[rng.below(globals.len() as u32) as usize];
                let slot = rng.below(2);
                let to = (rng.below(4) != 0).then(|| target.0.get(nogc));
                set_edge(nogc, g.get(nogc), slot, to);
                model.get_mut(gid).unwrap()[slot as usize] = to.map(|_| target.1);
            }
            // A new cell, held by a scoped root across a second allocation,
            // then made a global or hung off an existing cell.
            _ => cx.scope(|s| {
                let a = node(s, id, 2).bind(s.nogc());
                let a = s.root(a);
                let _garbage = node(s, 0xff_fff0, 0); // collects again
                let nogc = s.nogc();
                let a = a.get(nogc);
                let mut edges = [None, None];
                for (slot, e) in edges.iter_mut().enumerate() {
                    if !globals.is_empty() && rng.below(2) == 0 {
                        let (g, gid) = &globals[rng.below(globals.len() as u32) as usize];
                        set_edge(nogc, a, slot as u32, Some(g.get(nogc)));
                        *e = Some(*gid);
                    }
                }
                model.insert(id, edges);
                if globals.is_empty() || rng.below(3) != 0 {
                    globals.push((nogc.global(a), id));
                } else {
                    let (g, gid) = &globals[rng.below(globals.len() as u32) as usize];
                    let slot = rng.below(2);
                    set_edge(nogc, g.get(nogc), slot, Some(a));
                    model.get_mut(gid).unwrap()[slot as usize] = Some(id);
                }
            }),
        }
        // Compare everything reachable with the model.
        let nogc = cx.nogc();
        let mut stack: Vec<Gc<'_, Node>> = globals.iter().map(|(g, _)| g.get(nogc)).collect();
        let mut seen = std::collections::HashSet::new();
        while let Some(cell) = stack.pop() {
            let id = id_of(nogc, cell);
            if !seen.insert(id) {
                continue;
            }
            let expected = model[&id];
            for (slot, want) in expected.iter().enumerate() {
                let got = edge(nogc, cell, slot as u32);
                assert_eq!(got.map(|c| id_of(nogc, c)), *want, "step {id}");
                stack.extend(got);
            }
        }
    }
    drop(globals);
    cx.collect();
    assert_eq!(cx.allocated_bytes(), 0);
}

#[test]
fn swapping_the_context_out_of_a_scope_panics_instead_of_unrooting() {
    let mut a = checked(8, false);
    let mut b = checked(8, false);
    let err = catch_unwind(AssertUnwindSafe(|| {
        b.scope(|sb| {
            let x = node(sb, 1, 0).bind(sb.nogc());
            sb.root(x);
            a.scope(|sa| std::mem::swap(&mut **sa, &mut **sb));
        })
    }))
    .unwrap_err();
    let msg = panic_msg(err);
    assert!(msg.contains("different context"), "{msg}");
    // Nothing of the other context's stack was truncated.
    assert_eq!(a.handle_count() + b.handle_count(), 1);
}

#[test]
fn rooting_through_a_scope_whose_context_was_swapped_panics() {
    let mut a = checked(8, false);
    let mut b = checked(8, false);
    let err = catch_unwind(AssertUnwindSafe(|| {
        a.scope(|sa| {
            std::mem::swap(&mut **sa, &mut b);
            let x = node(sa, 1, 0).bind(sa.nogc());
            sa.root(x);
        })
    }))
    .unwrap_err();
    assert!(panic_msg(err).contains("swapped out"));
}

#[test]
fn stress_mode_checks_binding_even_with_the_verifier_off() {
    let cage = Cage::new(8 * C).expect("reserve a heap");
    let heap = Heap::from_cage(
        cage,
        HeapOptions {
            poison: false,
            verify: false,
        },
    );
    let mut cx = Cx::new(heap, Box::new(TestTracer), CxOptions { gc_stress: true });
    let stale = node(&mut cx, 1, 0);
    let _ = node(&mut cx, 2, 8);
    let err = catch_unwind(AssertUnwindSafe(|| stale.bind(cx.nogc()))).unwrap_err();
    assert!(panic_msg(err).contains("stale handle"));
}

#[test]
fn an_outer_root_can_be_set_while_an_inner_scope_is_open() {
    let mut cx = checked(8, false);
    cx.scope(|outer| {
        let a = node(outer, 1, 0).bind(outer.nogc());
        let r = outer.root(a);
        outer.scope(|inner| {
            let b = node(inner, 2, 0).bind(inner.nogc());
            r.set(inner.nogc(), b);
        });
        outer.collect();
        assert_eq!(id_of(outer.nogc(), r.get(outer.nogc())), 2);
        assert_eq!(outer.allocated_bytes(), 16);
    });
}
