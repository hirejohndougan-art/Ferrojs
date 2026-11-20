use std::collections::HashMap;

use super::*;
use crate::cage::NULL_REGION;

const C: u32 = CHUNK_SIZE;

/// A heap over a four-chunk cage: chunks 1..=3 are usable.
fn small() -> Heap {
    Heap::new(4 * C as u64).expect("reserve a small heap")
}

/// A heap with poisoning and the verifier on, over `chunks` chunks.
fn checked(chunks: u64) -> Heap {
    let cage = Cage::new(chunks * C as u64).expect("reserve a heap");
    Heap::from_cage(
        cage,
        HeapOptions {
            poison: true,
            verify: true,
        },
    )
}

// The test cell layout: header word = kind 2 | id << 8 | edge count << 32,
// then one u32 reference per edge.
const TEST_KIND: u8 = 2;

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

fn new_cell(heap: &mut Heap, id: u32, edges: &[u32]) -> u32 {
    let len = edges.len() as u32;
    let at = heap.alloc(8 + 4 * len).unwrap();
    heap.write_u64(at, TEST_KIND as u64 | (id as u64) << 8 | (len as u64) << 32);
    for (i, &e) in edges.iter().enumerate() {
        heap.write_u32(at + 8 + 4 * i as u32, e);
    }
    at
}

fn set_edge(heap: &mut Heap, cell: u32, i: u32, target: u32) {
    heap.write_u32(cell + 8 + 4 * i, target);
}

fn id_of(heap: &Heap, cell: u32) -> u32 {
    (heap.cage().read_u64(cell) >> 8) as u32 & 0xff_ffff
}

// ---- allocation ----

#[test]
fn header_size_leaves_room_for_the_mark_bitmap() {
    assert_eq!(CHUNK_HEADER_SIZE, 64 + 4096);
    assert_eq!(CHUNK_HEADER_SIZE % CELL_ALIGN, 0);
}

#[test]
fn size_classes_cover_every_small_size() {
    for rounded in (8..=LARGE_CELL).step_by(8) {
        let class = CLASS_OF[(rounded / 8) as usize] as usize;
        assert!(SIZE_CLASSES[class] >= rounded, "{rounded}");
        assert!(class == 0 || SIZE_CLASSES[class - 1] < rounded, "{rounded}");
    }
    assert!(SIZE_CLASSES.iter().all(|s| s % CELL_ALIGN == 0));
}

#[test]
fn cells_are_aligned_nonnull_and_zeroed() {
    let mut heap = small();
    for size in [1, 7, 8, 9, 16, 100, LARGE_CELL] {
        let at = heap.alloc(size).unwrap();
        assert!(at >= NULL_REGION, "{at:#x}");
        assert_eq!(at % CELL_ALIGN, 0);
        let mut bytes = vec![0xffu8; size as usize];
        heap.cage().read_bytes(at, &mut bytes);
        assert!(bytes.iter().all(|&b| b == 0));
        assert!(heap.is_live_cell(at));
    }
    assert_eq!(heap.allocated_bytes(), 5 * 16 + 128 + 8192);
}

#[test]
fn first_chunk_starts_after_its_header() {
    let mut heap = small();
    assert_eq!(heap.alloc(16).unwrap(), C + CHUNK_HEADER_SIZE);
    assert_eq!(heap.alloc(16).unwrap(), C + CHUNK_HEADER_SIZE + 16);
    let header = heap.cage().read_u64(C);
    assert_eq!(ChunkKind::from_header(header), Some(ChunkKind::Small));
    assert_eq!((header >> 8) as u8, 0, "size class 0");
    // Each size class gets its own chunk.
    assert_eq!(heap.alloc(24).unwrap(), 2 * C + CHUNK_HEADER_SIZE);
}

#[test]
fn cells_do_not_overlap() {
    let mut heap = small();
    let mut cells = Vec::new();
    // Enough 24-byte cells to spill into a second chunk.
    let per_chunk = (C - CHUNK_HEADER_SIZE) / 24;
    for i in 0..per_chunk + 10 {
        let at = heap.alloc(24).unwrap();
        // The first byte is the kind; 0xff would mark the cell freed.
        heap.write_u64(at, (i as u64) << 8);
        heap.write_u64(at + 16, !(i as u64));
        cells.push(at);
    }
    assert_eq!(heap.cage().committed_chunks(), 2);
    for (i, &at) in cells.iter().enumerate() {
        assert_eq!(heap.cage().read_u64(at), (i as u64) << 8);
        assert_eq!(heap.cage().read_u64(at + 16), !(i as u64));
    }
    // The cell that didn't fit went to the start of the next chunk.
    assert_eq!(cells[per_chunk as usize], 2 * C + CHUNK_HEADER_SIZE);
}

#[test]
fn the_last_cell_of_a_chunk_ends_inside_it() {
    let mut heap = small();
    let per_chunk = (C - CHUNK_HEADER_SIZE) / LARGE_CELL;
    let mut last = 0;
    for _ in 0..per_chunk {
        last = heap.alloc(LARGE_CELL).unwrap();
    }
    assert!(last + LARGE_CELL <= 2 * C);
    heap.fill(last, LARGE_CELL, 5);
    assert_eq!(heap.cage().committed_chunks(), 1);
    assert_eq!(heap.alloc(LARGE_CELL).unwrap(), 2 * C + CHUNK_HEADER_SIZE);
}

#[test]
fn large_cells_get_their_own_chunk_run() {
    let mut heap = small();
    let small_cell = heap.alloc(32).unwrap();
    let big = C; // needs header + 256 KB: a two-chunk run
    let at = heap.alloc(big).unwrap();
    assert_eq!(at, 2 * C + CHUNK_HEADER_SIZE);
    let header = heap.cage().read_u64(2 * C);
    assert_eq!(ChunkKind::from_header(header), Some(ChunkKind::LargeRun));
    assert_eq!(header >> 32, 2);
    assert_eq!(heap.cell_size(at), Some(big));
    assert!(!heap.is_live_cell(3 * C + CHUNK_HEADER_SIZE), "run tail");
    heap.fill(at, big, 0x11);
    assert_eq!(heap.cage().read_u8(at + big - 1), 0x11);
    // Small cells keep bumping in their own chunk, untouched by the run.
    let next = heap.alloc(32).unwrap();
    assert_eq!(next, small_cell + 32);
    assert_eq!(heap.cage().read_u64(small_cell), 0);
}

#[test]
fn running_out_is_an_error_not_a_panic() {
    let mut heap = small();
    // Three usable chunks; a run of three takes them all.
    heap.alloc(2 * C).unwrap();
    assert_eq!(heap.cage().committed_chunks(), 3);
    assert_eq!(
        heap.alloc(16).unwrap_err(),
        AllocError::OutOfMemory { size: 16 }
    );
    assert_eq!(
        heap.alloc(4 * C).unwrap_err(),
        AllocError::OutOfMemory { size: 4 * C }
    );
    // The error reports the size asked for, not the rounded one.
    assert_eq!(
        heap.alloc(4 * C - 3).unwrap_err(),
        AllocError::OutOfMemory { size: 4 * C - 3 }
    );
}

#[test]
fn bad_sizes_are_errors() {
    let mut heap = small();
    assert_eq!(heap.alloc(0).unwrap_err(), AllocError::BadSize(0));
    assert_eq!(
        heap.alloc(u32::MAX).unwrap_err(),
        AllocError::BadSize(u32::MAX as u64)
    );
    assert_eq!(heap.cage().committed_chunks(), 0);
}

#[cfg(not(miri))]
#[test]
fn biggest_cell_in_a_full_cage_is_out_of_memory() {
    let mut heap = Heap::new(crate::cage::MAX_CAGE_SIZE).unwrap();
    // Rounds up to 2^32, which can't fit beside chunk 0 and the header.
    assert_eq!(
        heap.alloc(u32::MAX).unwrap_err(),
        AllocError::OutOfMemory { size: u32::MAX }
    );
    assert!(heap.alloc(16).is_ok());
}

#[cfg(not(miri))]
#[test]
fn the_last_chunk_of_a_full_cage_sweeps() {
    // The verifier is off: skipping ahead leaves never-used chunks below
    // `next_chunk`, which it would rightly flag.
    let mut heap = Heap::from_cage(
        Cage::new(crate::cage::MAX_CAGE_SIZE).unwrap(),
        HeapOptions {
            poison: true,
            verify: false,
        },
    );
    // Push the bump chunk for class 8192 to the very last chunk.
    heap.next_chunk = heap.cage().chunk_count() - 1;
    let per_chunk = (C - CHUNK_HEADER_SIZE) / LARGE_CELL;
    let cells: Vec<u32> = (0..per_chunk)
        .map(|_| heap.alloc(LARGE_CELL).unwrap())
        .collect();
    assert!(cells
        .iter()
        .all(|&c| c / C == heap.cage().chunk_count() - 1));
    let stats = heap.collect([cells[0]], &EmptyTracer);
    assert_eq!(stats.live_bytes, LARGE_CELL as u64);
    assert!(heap.is_live_cell(cells[0]) && !heap.is_live_cell(cells[1]));
}

#[test]
#[should_panic(expected = "already in use")]
fn from_cage_refuses_a_used_cage() {
    let mut cage = Cage::new(4 * C as u64).unwrap();
    cage.commit_chunks(1, 1).unwrap();
    Heap::from_cage(cage, HeapOptions::default());
}

// ---- collection ----

/// For cells without references.
struct EmptyTracer;

impl Trace for EmptyTracer {
    fn trace(&self, _: &Cage, _: u32, _: u32, _: &mut Vec<u32>) {}
}

#[test]
fn unreachable_cells_are_freed_and_reachable_ones_kept() {
    let mut heap = checked(8);
    let a = new_cell(&mut heap, 1, &[0]);
    let b = new_cell(&mut heap, 2, &[0]);
    let c = new_cell(&mut heap, 3, &[a]);
    set_edge(&mut heap, a, 0, b);
    set_edge(&mut heap, b, 0, c); // a -> b -> c -> a
    let d = new_cell(&mut heap, 4, &[a]); // points in, but nothing points at d
    let e = new_cell(&mut heap, 5, &[]);
    let arr = new_cell(&mut heap, 6, &[0, e, 0, 0, 0, 0, 0, 0]);

    let stats = heap.collect([a, arr], &TestTracer);
    for cell in [a, b, c, e, arr] {
        assert!(heap.is_live_cell(cell), "{cell:#x}");
    }
    assert_eq!([id_of(&heap, a), id_of(&heap, c)], [1, 3]);
    assert_eq!(heap.cage().read_u32(c + 8), a);
    assert!(!heap.is_live_cell(d));
    assert_eq!(heap.cage().read_u8(d), FREED_KIND);
    assert_eq!(heap.cage().read_u8(d + 1), POISON_BYTE);
    assert_eq!(stats.freed_bytes, 16);
    assert_eq!(stats.live_bytes, 4 * 16 + 48);
    assert_eq!(heap.allocated_bytes(), stats.live_bytes);
}

#[test]
fn freed_cells_are_reused_and_zeroed() {
    let mut heap = checked(8);
    let keep = new_cell(&mut heap, 1, &[]);
    let x = new_cell(&mut heap, 2, &[]);
    heap.collect([keep], &TestTracer);
    assert!(!heap.is_live_cell(x));
    let y = heap.alloc(16).unwrap();
    assert_eq!(y, x);
    assert_eq!(heap.cage().read_u64(y), 0);
    assert_eq!(heap.cage().read_u64(y + 8), 0);
    assert!(heap.is_live_cell(y));
}

#[test]
fn empty_chunks_return_to_the_pool_and_come_back_zeroed() {
    let mut heap = checked(8);
    let per_chunk = (C - CHUNK_HEADER_SIZE) / 16;
    for i in 0..per_chunk + 1 {
        new_cell(&mut heap, i, &[]);
    }
    // Chunk 1 is full, chunk 2 is the bump chunk for 16-byte cells.
    let stats = heap.collect([], &TestTracer);
    assert_eq!(stats.freed_chunks, 1, "the bump chunk is kept");
    assert_eq!(stats.live_bytes, 0);
    // A different size class takes chunk 1 over.
    let at = heap.alloc(100).unwrap();
    assert_eq!(at, C + CHUNK_HEADER_SIZE);
    assert_eq!((heap.cage().read_u64(C) >> 8) as u8, 6, "class of 128");
    let mut bytes = vec![1u8; 128];
    heap.cage().read_bytes(at, &mut bytes);
    assert!(bytes.iter().all(|&b| b == 0));
    // The 16-byte class reuses its own free cells in chunk 2 first.
    assert_eq!(heap.alloc(16).unwrap() / C, 2);
}

#[test]
fn large_runs_are_freed_and_reused() {
    let mut heap = checked(8);
    let keep = heap.alloc(20_000).unwrap();
    let big = heap.alloc(C).unwrap();
    heap.fill(big, C, 0x77);
    let stats = heap.collect([keep], &EmptyTracer);
    assert_eq!(stats.freed_chunks, 2);
    assert_eq!(stats.freed_bytes, C as u64);
    assert!(heap.is_live_cell(keep) && !heap.is_live_cell(big));
    let again = heap.alloc(C - 8).unwrap();
    assert_eq!(again, big);
    assert_eq!(heap.cage().read_u64(again + C - 16), 0);
    // Two free single chunks that aren't adjacent can't hold a run of two.
    assert_eq!(heap.find_free_run(1), None);
}

#[test]
fn free_runs_must_be_contiguous() {
    let mut heap = small();
    heap.free_chunks.extend([1, 3]);
    assert_eq!(heap.find_free_run(1), Some(1));
    assert_eq!(heap.find_free_run(2), None);
    heap.free_chunks.insert(2);
    assert_eq!(heap.find_free_run(3), Some(1));
}

#[test]
fn deep_chains_mark_without_recursion() {
    let n: u32 = if cfg!(miri) { 300 } else { 200_000 };
    let mut heap = checked(64);
    let mut head = 0;
    for i in 0..n {
        head = new_cell(&mut heap, i, &[head]);
    }
    let stats = heap.collect([head], &TestTracer);
    assert_eq!(stats.live_bytes, n as u64 * 16);
    let stats = heap.collect([], &TestTracer);
    assert_eq!(stats.freed_bytes, n as u64 * 16);
    assert_eq!(heap.allocated_bytes(), 0);
}

#[test]
fn collection_is_due_after_enough_allocation() {
    let mut heap = checked(16);
    while !heap.wants_collection() {
        heap.alloc(1024).unwrap();
    }
    assert_eq!(heap.allocated_bytes(), MIN_GC_THRESHOLD);
    heap.collect([], &EmptyTracer);
    assert!(!heap.wants_collection());
}

#[test]
#[should_panic(expected = "heap verifier")]
fn the_verifier_catches_a_dangling_reference() {
    let mut heap = checked(8);
    let a = new_cell(&mut heap, 1, &[0]);
    let b = new_cell(&mut heap, 2, &[]);
    heap.collect([a], &TestTracer);
    set_edge(&mut heap, a, 0, b); // b was freed
    heap.verify([a], &TestTracer);
}

#[test]
#[should_panic(expected = "does not point at a live cell")]
fn an_interior_reference_panics() {
    let mut heap = checked(8);
    let a = new_cell(&mut heap, 1, &[]);
    heap.collect([a + 8], &TestTracer);
}

#[test]
#[should_panic(expected = "does not point at a live cell")]
fn a_root_to_a_freed_cell_panics() {
    let mut heap = checked(8);
    let a = new_cell(&mut heap, 1, &[]);
    heap.collect([], &TestTracer);
    heap.collect([a], &TestTracer);
}

/// Collect after every allocation of a random mutator, checking the heap
/// against a model of the object graph each time.
#[test]
fn stress_collect_on_every_allocation() {
    let steps: u32 = if cfg!(miri) { 60 } else { 3000 };
    let mut heap = checked(if cfg!(miri) { 32 } else { 64 });
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u32| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n.max(1) as u64) as u32
    };
    // offset -> (id, edges) for every cell the model believes is alive.
    let mut model: HashMap<u32, (u32, Vec<u32>)> = HashMap::new();
    let mut roots: Vec<u32> = Vec::new();
    for step in 0..steps {
        // Mostly small cells, sometimes a large one.
        let len = match next(20) {
            0 => 2100 + next(1000),
            1..=4 => next(300),
            _ => next(4),
        };
        let live: Vec<u32> = model.keys().copied().collect();
        let edges: Vec<u32> = (0..len)
            .map(|_| match next(3) {
                0 => 0,
                _ => live
                    .get(next(live.len() as u32) as usize)
                    .copied()
                    .unwrap_or(0),
            })
            .collect();
        let cell = new_cell(&mut heap, step, &edges);
        model.insert(cell, (step, edges));
        roots.push(cell);
        if roots.len() > 8 || next(3) == 0 {
            let i = next(roots.len() as u32) as usize;
            roots.swap_remove(i);
        }
        // Occasionally rewire an edge of a live cell.
        if next(4) == 0 {
            let cells: Vec<u32> = model.keys().copied().collect();
            let from = cells[next(cells.len() as u32) as usize];
            let to = cells[next(cells.len() as u32) as usize];
            let entry = model.get_mut(&from).unwrap();
            if !entry.1.is_empty() {
                let i = next(entry.1.len() as u32);
                entry.1[i as usize] = to;
                set_edge(&mut heap, from, i, to);
            }
        }

        heap.collect(roots.iter().copied(), &TestTracer);

        // What the model says is reachable must be intact and live.
        let mut seen = std::collections::HashSet::new();
        let mut stack = roots.clone();
        while let Some(c) = stack.pop() {
            if c == 0 || !seen.insert(c) {
                continue;
            }
            let (id, edges) = &model[&c];
            assert!(heap.is_live_cell(c), "step {step}: {c:#x} was freed");
            assert_eq!(id_of(&heap, c), *id, "step {step}");
            for (i, &e) in edges.iter().enumerate() {
                assert_eq!(heap.cage().read_u32(c + 8 + 4 * i as u32), e);
                stack.push(e);
            }
        }
        // Everything else must have been freed.
        model.retain(|&c, _| {
            let reachable = seen.contains(&c);
            assert_eq!(heap.is_live_cell(c), reachable, "step {step}: {c:#x}");
            reachable
        });
        let expected: u64 = model
            .values()
            .map(|(_, e)| {
                let rounded = (8 + 4 * e.len() as u32).next_multiple_of(8);
                if rounded <= LARGE_CELL {
                    SIZE_CLASSES[CLASS_OF[(rounded / 8) as usize] as usize] as u64
                } else {
                    rounded as u64
                }
            })
            .sum();
        assert_eq!(heap.allocated_bytes(), expected, "step {step}");
    }
}

/// Panics when it traces the cell with id 99; otherwise like `TestTracer`.
struct PanickyTracer;

impl Trace for PanickyTracer {
    fn trace(&self, cage: &Cage, cell: u32, size: u32, edges: &mut Vec<u32>) {
        assert_ne!((cage.read_u64(cell) >> 8) as u32 & 0xff_ffff, 99, "boom");
        TestTracer.trace(cage, cell, size, edges)
    }
}

#[test]
fn a_collection_that_panicked_leaves_no_stale_mark_bits() {
    let mut heap = checked(8);
    let p = new_cell(&mut heap, 99, &[]);
    let a = new_cell(&mut heap, 1, &[0]);
    // Roots are marked in order and traced last-first: `a` is marked and
    // traced, then tracing `p` panics.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        heap.collect([p, a], &PanickyTracer);
    }));
    assert!(result.is_err());
    let c = new_cell(&mut heap, 2, &[]);
    set_edge(&mut heap, a, 0, c);
    heap.collect([a], &TestTracer);
    assert!(heap.is_live_cell(c), "reachable cell freed after a panic");
    assert!(!heap.is_live_cell(p));
}

#[test]
#[should_panic(expected = "not inside one live cell")]
fn writing_a_chunk_header_panics() {
    let mut heap = checked(8);
    let a = new_cell(&mut heap, 1, &[]);
    let (word, mask) = Heap::mark_bit(a);
    heap.write_u64(word, mask);
}

#[test]
#[should_panic(expected = "not inside one live cell")]
fn writing_a_freed_cell_panics() {
    let mut heap = checked(8);
    let a = new_cell(&mut heap, 1, &[]);
    heap.collect([], &TestTracer);
    heap.write_u64(a + 8, 1);
}

#[test]
#[should_panic(expected = "not inside one live cell")]
fn writing_past_the_bump_pointer_panics() {
    let mut heap = checked(8);
    let a = new_cell(&mut heap, 1, &[]);
    heap.write_u64(a + 16, 1);
}

#[test]
#[should_panic(expected = "not inside one live cell")]
fn a_write_running_past_its_cell_panics() {
    let mut heap = checked(8);
    let a = heap.alloc(24).unwrap();
    heap.alloc(24).unwrap();
    heap.write_bytes(a + 16, &[0; 16]);
}

#[test]
fn writes_reach_every_chunk_of_a_large_cell() {
    let mut heap = checked(8);
    let big = heap.alloc(2 * C).unwrap();
    heap.write_u64(big + 2 * C - 8, 7);
    heap.write_u8(big + C, 1);
    assert_eq!(heap.cage().read_u64(big + 2 * C - 8), 7);
}

#[test]
fn a_run_can_start_in_the_pool_and_end_in_fresh_chunks() {
    let mut heap = checked(4);
    let keep = heap.alloc(16).unwrap(); // chunk 1
    let big = heap.alloc(100_000).unwrap(); // chunk 2, a one-chunk run
    heap.fill(big, 100_000, 9);
    heap.collect([keep], &EmptyTracer);
    // Chunk 2 is pooled and chunk 3 never committed: together they fit.
    let run = heap.alloc(C).unwrap();
    assert_eq!(run, big);
    assert_eq!(heap.cage().read_u64(run + 99_992), 0, "reused part zeroed");
    assert_eq!(heap.cage().committed_chunks(), 3);
    heap.verify([keep, run], &EmptyTracer);
}

/// Like `stress_collect_on_every_allocation`, but collecting only now and
/// then, so garbage piles up, and with cells spanning several chunks.
#[test]
fn stress_periodic_collection_with_chunk_runs() {
    let steps: u32 = if cfg!(miri) { 80 } else { 2500 };
    let mut heap = checked(if cfg!(miri) { 32 } else { 96 });
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |n: u32| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n.max(1) as u64) as u32
    };
    let mut model: HashMap<u32, (u32, Vec<u32>)> = HashMap::new();
    let mut roots: Vec<u32> = Vec::new();
    for step in 0..steps {
        let len = match next(40) {
            0 if !cfg!(miri) => 70_000 + next(150_000), // 1 to 4 chunks
            1..=3 => 2100 + next(3000),
            4..=10 => next(300),
            _ => next(4),
        };
        let live: Vec<u32> = model.keys().copied().collect();
        let edges: Vec<u32> = (0..len.min(64))
            .map(|_| {
                live.get(next(live.len() as u32) as usize)
                    .copied()
                    .unwrap_or(0)
            })
            .chain(std::iter::repeat(0).take(len.saturating_sub(64) as usize))
            .collect();
        let cell = new_cell(&mut heap, step & 0xff_ffff, &edges);
        model.insert(cell, (step & 0xff_ffff, edges));
        roots.push(cell);
        if roots.len() > 6 || next(2) == 0 {
            let i = next(roots.len() as u32) as usize;
            roots.swap_remove(i);
        }
        if next(25) != 0 {
            continue;
        }
        heap.collect(roots.iter().copied(), &TestTracer);
        let mut seen = std::collections::HashSet::new();
        let mut stack = roots.clone();
        while let Some(c) = stack.pop() {
            if c == 0 || !seen.insert(c) {
                continue;
            }
            let (id, edges) = &model[&c];
            assert!(heap.is_live_cell(c), "step {step}: {c:#x} was freed");
            assert_eq!(id_of(&heap, c), *id, "step {step}");
            stack.extend(edges.iter().copied());
        }
        model.retain(|&c, _| {
            let reachable = seen.contains(&c);
            assert_eq!(heap.is_live_cell(c), reachable, "step {step}: {c:#x}");
            reachable
        });
    }
}

#[test]
#[should_panic(expected = "not inside one live cell")]
fn the_cheap_write_check_still_guards_chunk_headers() {
    let cage = Cage::new(4 * C as u64).unwrap();
    let mut heap = Heap::from_cage(
        cage,
        HeapOptions {
            poison: false,
            verify: false,
        },
    );
    let a = heap.alloc(16).unwrap();
    heap.write_u64(a, 1); // fine
    let (word, mask) = Heap::mark_bit(a);
    heap.write_u64(word, mask);
}
