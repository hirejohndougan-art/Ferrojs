use super::*;

/// A small cage: four chunks (1 MB), so tests are quick under Miri.
fn small() -> Cage {
    Cage::new(4 * CHUNK_SIZE as u64).expect("reserve a small cage")
}

const C: u32 = CHUNK_SIZE;

#[test]
fn rejects_bad_sizes() {
    for size in [
        0,
        CHUNK_SIZE as u64,
        3 * CHUNK_SIZE as u64 + 8,
        MAX_CAGE_SIZE + CHUNK_SIZE as u64,
    ] {
        assert_eq!(Cage::new(size).unwrap_err(), CageError::BadSize(size));
    }
}

#[test]
fn default_cage_reserves_without_committing() {
    let cage = Cage::with_default_size().expect("reserve the default cage");
    assert_eq!(cage.size(), DEFAULT_CAGE_SIZE);
    assert_eq!(cage.committed_chunks(), 0);
    assert!(!cage.is_accessible(NULL_REGION, 8));
}

#[test]
fn commit_makes_zeroed_memory_readable_and_writable() {
    let mut cage = small();
    assert!(!cage.is_accessible(C, 8));
    cage.commit_chunks(1, 2).unwrap();
    assert_eq!(cage.committed_chunks(), 2);
    assert!(cage.is_chunk_committed(1) && cage.is_chunk_committed(2));
    assert!(!cage.is_chunk_committed(3));
    // Fresh memory is zero, at both ends of the committed range.
    assert_eq!(cage.read_u64(C), 0);
    assert_eq!(cage.read_u64(3 * C - 8), 0);

    cage.write_u64(C, 0x0123_4567_89ab_cdef);
    assert_eq!(cage.read_u64(C), 0x0123_4567_89ab_cdef);
    assert_eq!(cage.read_u32(C), 0x89ab_cdef);
    assert_eq!(cage.read_u32(C + 4), 0x0123_4567);
    assert_eq!(cage.read_u8(C), 0xef);

    cage.write_u32(C + 12, 0xdead_beef);
    cage.write_u8(C + 9, 7);
    assert_eq!(cage.read_u64(C + 8), 0xdead_beef_0000_0700);
}

#[test]
fn byte_copies_and_fill_cross_chunk_boundaries() {
    let mut cage = small();
    cage.commit_chunks(1, 2).unwrap();
    let at = 2 * C - 3;
    cage.write_bytes(at, b"abcdef");
    let mut out = [0u8; 6];
    cage.read_bytes(at, &mut out);
    assert_eq!(&out, b"abcdef");
    cage.fill(at + 1, 4, 0xa5);
    cage.read_bytes(at, &mut out);
    assert_eq!(out, [b'a', 0xa5, 0xa5, 0xa5, 0xa5, b'f']);
    // Empty accesses are no-ops, even at offsets that aren't accessible.
    cage.read_bytes(0, &mut []);
    cage.write_bytes(0, &[]);
    cage.fill(0, 0, 1);
}

#[test]
fn last_bytes_of_the_cage_are_usable() {
    let mut cage = small();
    assert!(!cage.is_full_size());
    let last = cage.chunk_count() - 1;
    cage.commit_chunks(last, 1).unwrap();
    let end = cage.size() as u32;
    cage.write_u64(end - 8, u64::MAX);
    assert_eq!(cage.read_u64(end - 8), u64::MAX);
    assert!(!cage.is_accessible(end - 8, 9));
    assert!(!cage.is_accessible(end, 1));
}

#[cfg(not(miri))]
#[test]
fn last_bytes_of_a_full_4gb_cage_are_usable() {
    let mut cage = Cage::new(MAX_CAGE_SIZE).expect("reserve a 4 GB cage");
    assert!(cage.is_full_size());
    let last = cage.chunk_count() - 1;
    assert_eq!(last as u64, MAX_CAGE_SIZE / C as u64 - 1);
    cage.commit_chunks(last, 1).unwrap();
    cage.write_u64(u32::MAX - 7, 42);
    assert_eq!(cage.read_u64(u32::MAX - 7), 42);
    cage.write_u8(u32::MAX, 9);
    assert_eq!(cage.read_u8(u32::MAX), 9);
    // Lengths that would run past 2^32 are rejected, not wrapped.
    assert!(!cage.is_accessible(u32::MAX, 2));
    assert!(!cage.is_accessible(u32::MAX - 7, u32::MAX));
}

#[test]
fn bad_commits_are_refused() {
    let mut cage = small();
    let bad = |first, count| CageError::BadChunkRange { first, count };
    assert_eq!(cage.commit_chunks(0, 1).unwrap_err(), bad(0, 1));
    assert_eq!(cage.commit_chunks(1, 0).unwrap_err(), bad(1, 0));
    assert_eq!(cage.commit_chunks(3, 2).unwrap_err(), bad(3, 2));
    assert_eq!(cage.commit_chunks(4, 1).unwrap_err(), bad(4, 1));
    assert_eq!(
        cage.commit_chunks(1, u32::MAX).unwrap_err(),
        bad(1, u32::MAX)
    );
    cage.commit_chunks(2, 1).unwrap();
    assert_eq!(
        cage.commit_chunks(1, 2).unwrap_err(),
        CageError::AlreadyCommitted(2)
    );
    // A refused commit changes nothing.
    assert!(!cage.is_chunk_committed(1));
    assert_eq!(cage.committed_chunks(), 1);
}

#[test]
fn null_region_is_never_accessible() {
    let mut cage = small();
    cage.commit_chunks(1, 3).unwrap();
    for offset in [0, 8, NULL_REGION - 8, C - 8] {
        assert!(!cage.is_accessible(offset, 8), "offset {offset:#x}");
    }
    assert!(!cage.is_accessible(C - 1, 2), "range starting in chunk 0");
}

#[test]
#[should_panic(expected = "outside committed memory")]
fn reading_null_panics() {
    let mut cage = small();
    cage.commit_chunks(1, 1).unwrap();
    cage.read_u64(0);
}

#[test]
#[should_panic(expected = "outside committed memory")]
fn writing_uncommitted_memory_panics() {
    let mut cage = small();
    cage.commit_chunks(1, 1).unwrap();
    cage.write_u64(2 * C, 1);
}

#[test]
#[should_panic(expected = "outside committed memory")]
fn copy_running_into_uncommitted_memory_panics() {
    let mut cage = small();
    cage.commit_chunks(1, 1).unwrap();
    cage.write_bytes(2 * C - 2, b"xyz");
}

#[test]
#[should_panic(expected = "outside committed memory")]
fn access_past_the_end_panics() {
    let mut cage = small();
    cage.commit_chunks(3, 1).unwrap();
    let mut buf = [0u8; 16];
    cage.read_bytes(4 * C - 8, &mut buf);
}

#[test]
#[should_panic(expected = "not 8-byte aligned")]
fn misaligned_u64_panics() {
    let mut cage = small();
    cage.commit_chunks(1, 1).unwrap();
    cage.read_u64(C + 4);
}

#[test]
#[should_panic(expected = "not 4-byte aligned")]
fn misaligned_u32_panics() {
    let mut cage = small();
    cage.commit_chunks(1, 1).unwrap();
    cage.write_u32(C + 2, 1);
}

#[test]
fn several_cages_coexist_and_drop() {
    let mut a = small();
    let mut b = small();
    a.commit_chunks(1, 1).unwrap();
    b.commit_chunks(1, 1).unwrap();
    a.write_u64(C, 1);
    b.write_u64(C, 2);
    assert_eq!((a.read_u64(C), b.read_u64(C)), (1, 2));
    drop(a);
    assert_eq!(b.read_u64(C), 2);
}
