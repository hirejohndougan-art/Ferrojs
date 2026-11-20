use super::*;
use crate::cage::NULL_REGION;

const C: u32 = CHUNK_SIZE;

/// A heap over a four-chunk cage: chunks 1..=3 are usable.
fn small() -> Heap {
    Heap::new(4 * C as u64).expect("reserve a small heap")
}

#[test]
fn header_size_leaves_room_for_the_mark_bitmap() {
    assert_eq!(CHUNK_HEADER_SIZE, 64 + 4096);
    assert_eq!(CHUNK_HEADER_SIZE % CELL_ALIGN, 0);
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
    }
    assert_eq!(heap.allocated_bytes(), 8 + 8 + 8 + 16 + 16 + 104 + 8192);
}

#[test]
fn first_chunk_starts_after_its_header() {
    let mut heap = small();
    assert_eq!(heap.alloc(16).unwrap(), C + CHUNK_HEADER_SIZE);
    assert_eq!(heap.alloc(16).unwrap(), C + CHUNK_HEADER_SIZE + 16);
    let header = heap.cage().read_u64(C);
    assert_eq!(ChunkKind::from_header(header), Some(ChunkKind::Bump));
}

#[test]
fn cells_do_not_overlap() {
    let mut heap = small();
    let mut cells = Vec::new();
    // Enough 24-byte cells to spill into a second chunk.
    let per_chunk = (C - CHUNK_HEADER_SIZE) / 24;
    for i in 0..per_chunk + 10 {
        let at = heap.alloc(24).unwrap();
        heap.write_u64(at, i as u64);
        heap.write_u64(at + 16, !(i as u64));
        cells.push(at);
    }
    assert_eq!(heap.cage().committed_chunks(), 2);
    for (i, &at) in cells.iter().enumerate() {
        assert_eq!(heap.cage().read_u64(at), i as u64);
        assert_eq!(heap.cage().read_u64(at + 16), !(i as u64));
    }
    // The cell that didn't fit went to the start of the next chunk.
    assert_eq!(cells[per_chunk as usize], 2 * C + CHUNK_HEADER_SIZE);
}

#[test]
fn a_cell_exactly_filling_the_chunk_tail_fits() {
    let mut heap = small();
    let room = C - CHUNK_HEADER_SIZE;
    // Fill all but the last 8 KB, then take exactly the rest.
    let mut used = 0;
    while room - used > LARGE_CELL {
        heap.alloc(LARGE_CELL).unwrap();
        used += LARGE_CELL;
    }
    let tail = room - used;
    let at = heap.alloc(tail).unwrap();
    assert_eq!(at + tail, 2 * C);
    heap.write_u64(2 * C - 8, 5);
    assert_eq!(heap.cage().committed_chunks(), 1);
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
    let fill = 2 * C;
    heap.alloc(fill).unwrap();
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

#[test]
#[should_panic(expected = "already in use")]
fn from_cage_refuses_a_used_cage() {
    let mut cage = Cage::new(4 * C as u64).unwrap();
    cage.commit_chunks(1, 1).unwrap();
    Heap::from_cage(cage);
}
