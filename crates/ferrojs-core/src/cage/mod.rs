//! The heap cage (D-002, D-022, D-027).
//!
//! One engine instance owns one cage: a single reservation of address
//! space (4 GB by default) plus a 64 KB inaccessible guard at its end.
//! Heap references are 32-bit byte offsets from the cage base, never raw
//! pointers. Memory is committed in 256 KB chunks as the allocator needs
//! it; chunk 0 is never committed, so offset 0 (the null reference) and the
//! whole low 64 KB always fault.
//!
//! Every access from safe code goes through checked accessors that refuse
//! offsets outside committed chunks, so a wrong offset panics instead of
//! touching memory. The facts those checks rely on (the base, the size and
//! which chunks are committed) are kept in this Rust struct, outside the
//! cage, so nothing written into the cage can change them.
//!
//! Faster tiers will later use `base + u32 + small displacement` directly;
//! with the full 4 GB size plus the trailing guard, every such address
//! stays inside the reservation. Smaller cages (tests, Miri) don't have
//! that property, so code that skips the checks must first require
//! [`Cage::is_full_size`].

// This module and its `sys` backends are where the cage's `unsafe` lives;
// the crate-wide `unsafe_code` warning stays on everywhere else.
#![allow(unsafe_code)]

mod sys;

use std::fmt;
use std::ptr::NonNull;

/// Size of a chunk: the unit of commitment and of allocator bookkeeping.
pub const CHUNK_SIZE: u32 = 256 * 1024;

/// Inaccessible bytes reserved after the usable part of the cage.
pub const GUARD_SIZE: u32 = 64 * 1024;

/// Offsets below this are never committed (offset 0 is null). Chunk 0
/// covers this region and is never handed out.
pub const NULL_REGION: u32 = 64 * 1024;

/// The largest cage: every `u32` offset is inside it.
pub const MAX_CAGE_SIZE: u64 = 1 << 32;

/// The cage size an engine instance reserves unless told otherwise. Under
/// Miri the reservation is a real allocation, so it is kept small there.
#[cfg(not(miri))]
pub const DEFAULT_CAGE_SIZE: u64 = MAX_CAGE_SIZE;
#[cfg(miri)]
pub const DEFAULT_CAGE_SIZE: u64 = 8 * CHUNK_SIZE as u64;

/// Why a cage operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CageError {
    /// The requested cage size is not a multiple of [`CHUNK_SIZE`], is
    /// smaller than two chunks, or is bigger than [`MAX_CAGE_SIZE`].
    BadSize(u64),
    /// The system refused to reserve the address space.
    ReserveFailed(u64),
    /// The chunk range is empty, includes chunk 0, or runs past the end.
    BadChunkRange { first: u32, count: u32 },
    /// A chunk in the range is already committed.
    AlreadyCommitted(u32),
    /// The system refused to commit the memory.
    CommitFailed { first: u32, count: u32 },
}

impl fmt::Display for CageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CageError::BadSize(s) => write!(
                f,
                "cage size {s} must be a multiple of {CHUNK_SIZE}, at least two chunks and at most {MAX_CAGE_SIZE}"
            ),
            CageError::ReserveFailed(s) => {
                write!(f, "could not reserve {s} bytes of address space")
            }
            CageError::BadChunkRange { first, count } => {
                write!(f, "bad chunk range {first}+{count}")
            }
            CageError::AlreadyCommitted(c) => write!(f, "chunk {c} is already committed"),
            CageError::CommitFailed { first, count } => {
                write!(f, "could not commit chunks {first}+{count}")
            }
        }
    }
}

impl std::error::Error for CageError {}

/// A reserved region of address space that heap offsets point into.
///
/// Not `Send` or `Sync`: a cage belongs to one engine instance on one
/// thread, so cage memory is never accessed concurrently. Safe code never
/// borrows cage memory: every read and write copies through a raw pointer
/// derived from the base. Public writes take `&mut self`; the heap also
/// writes through `&self` (crate-private `store_*`, D-030).
pub struct Cage {
    /// Start of the reservation. Every pointer into the cage is derived
    /// from this one (strict provenance), never made from an integer.
    base: NonNull<u8>,
    /// Usable bytes: a multiple of `CHUNK_SIZE`, at most `MAX_CAGE_SIZE`.
    size: u64,
    /// One bit per chunk, set once the chunk is committed.
    committed: Vec<u64>,
    /// Number of committed chunks.
    committed_chunks: u32,
}

impl Cage {
    /// Reserves a cage of `size` usable bytes plus the trailing guard.
    pub fn new(size: u64) -> Result<Cage, CageError> {
        if size % CHUNK_SIZE as u64 != 0 || size < 2 * CHUNK_SIZE as u64 || size > MAX_CAGE_SIZE {
            return Err(CageError::BadSize(size));
        }
        let len = Self::reservation_len(size).ok_or(CageError::ReserveFailed(size))?;
        let base = sys::reserve(len).ok_or(CageError::ReserveFailed(size))?;
        let chunks = (size / CHUNK_SIZE as u64) as usize;
        Ok(Cage {
            base,
            size,
            committed: vec![0; chunks.div_ceil(64)],
            committed_chunks: 0,
        })
    }

    /// Reserves a cage of [`DEFAULT_CAGE_SIZE`].
    pub fn with_default_size() -> Result<Cage, CageError> {
        Cage::new(DEFAULT_CAGE_SIZE)
    }

    fn reservation_len(size: u64) -> Option<usize> {
        usize::try_from(size.checked_add(GUARD_SIZE as u64)?).ok()
    }

    /// Usable bytes in the cage (not counting the guard).
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Whether every `u32` offset (plus a displacement under the guard
    /// size) lands inside the reservation: true only for a 4 GB cage.
    pub fn is_full_size(&self) -> bool {
        self.size == MAX_CAGE_SIZE
    }

    /// Number of chunks in the cage, including the never-used chunk 0.
    pub fn chunk_count(&self) -> u32 {
        (self.size / CHUNK_SIZE as u64) as u32
    }

    /// Number of chunks committed so far.
    pub fn committed_chunks(&self) -> u32 {
        self.committed_chunks
    }

    /// Whether chunk `index` is committed.
    pub fn is_chunk_committed(&self, index: u32) -> bool {
        let i = index as usize;
        match self.committed.get(i / 64) {
            Some(word) => word & (1 << (i % 64)) != 0,
            None => false,
        }
    }

    /// Commits `count` chunks starting at chunk `first`, making them
    /// readable and writable. Their memory reads as zero. Chunk 0 can
    /// never be committed.
    pub fn commit_chunks(&mut self, first: u32, count: u32) -> Result<(), CageError> {
        let bad = CageError::BadChunkRange { first, count };
        let end = first.checked_add(count).ok_or(bad.clone())?;
        if first == 0 || count == 0 || end > self.chunk_count() {
            return Err(bad);
        }
        if let Some(c) = (first..end).find(|&c| self.is_chunk_committed(c)) {
            return Err(CageError::AlreadyCommitted(c));
        }
        let offset = first as usize * CHUNK_SIZE as usize;
        let len = count as usize * CHUNK_SIZE as usize;
        // SAFETY: `first >= 1` and `end <= chunk_count`, so the byte range
        // `offset..offset + len` lies inside the usable part of our own
        // reservation, which stays alive until `drop`. Chunk boundaries are
        // multiples of 256 KB from a page-aligned base, so they are
        // page-aligned.
        let ok = unsafe { sys::commit(self.base.as_ptr().add(offset), len) };
        if !ok {
            return Err(CageError::CommitFailed { first, count });
        }
        for c in first..end {
            self.committed[c as usize / 64] |= 1 << (c % 64);
        }
        self.committed_chunks += count;
        Ok(())
    }

    /// Whether `len` bytes at `offset` are all inside committed chunks.
    pub fn is_accessible(&self, offset: u32, len: u32) -> bool {
        if len == 0 {
            return false;
        }
        let end = offset as u64 + len as u64; // exclusive; cannot overflow u64
        if end > self.size {
            return false;
        }
        let first = offset / CHUNK_SIZE;
        let last = ((end - 1) / CHUNK_SIZE as u64) as u32;
        (first..=last).all(|c| self.is_chunk_committed(c))
    }

    /// Returns a pointer to `len` committed bytes at `offset` aligned to
    /// `align`, or panics. The pointer is derived from `base`.
    #[inline]
    fn checked_ptr(&self, offset: u32, len: u32, align: u32) -> *mut u8 {
        assert!(
            offset % align == 0,
            "cage access at {offset:#x} is not {align}-byte aligned"
        );
        assert!(
            self.is_accessible(offset, len),
            "cage access of {len} bytes at {offset:#x} is outside committed memory"
        );
        // SAFETY: `is_accessible` checked that `offset + len <= size`, and
        // the reservation is `size + GUARD_SIZE` bytes long, so the result
        // stays inside the same allocation as `base`.
        unsafe { self.base.as_ptr().add(offset as usize) }
    }

    /// Reads one byte.
    pub fn read_u8(&self, offset: u32) -> u8 {
        let p = self.checked_ptr(offset, 1, 1);
        // SAFETY: `checked_ptr` proved the byte is inside a committed
        // chunk. The cage is neither `Send` nor `Sync` and safe code never
        // holds a Rust reference into cage memory (every access copies), so
        // no write can race with or alias this read.
        unsafe { p.read() }
    }

    /// Reads a `u32` at a 4-byte-aligned offset.
    pub fn read_u32(&self, offset: u32) -> u32 {
        let p = self.checked_ptr(offset, 4, 4);
        // SAFETY: four committed bytes at a 4-aligned offset from a
        // page-aligned base, so the pointer is aligned for `u32`; every bit
        // pattern is a valid `u32`. No racing or aliasing access, as in
        // `read_u8`.
        unsafe { p.cast::<u32>().read() }
    }

    /// Reads a `u64` at an 8-byte-aligned offset.
    pub fn read_u64(&self, offset: u32) -> u64 {
        let p = self.checked_ptr(offset, 8, 8);
        // SAFETY: as in `read_u32`, for eight bytes at an 8-aligned offset.
        unsafe { p.cast::<u64>().read() }
    }

    // Writes come in two forms. The public ones take `&mut self`, so code
    // outside this crate that holds a `&Cage` (a `Trace` callback, say) can
    // only read. The `store_*` forms take `&self` for the heap's checked
    // writes through shared GC handles (D-030). Both are sound for the
    // same reason as the reads: the cage is not `Sync`, so all access is
    // on one thread, and cage memory is only ever reached through raw
    // pointers derived from `base`, never through Rust references, so a
    // write through `&self` invalidates nothing.

    /// Writes one byte.
    pub fn write_u8(&mut self, offset: u32, value: u8) {
        self.store_u8(offset, value)
    }

    /// Writes a `u32` at a 4-byte-aligned offset.
    pub fn write_u32(&mut self, offset: u32, value: u32) {
        self.store_u32(offset, value)
    }

    /// Writes a `u64` at an 8-byte-aligned offset.
    pub fn write_u64(&mut self, offset: u32, value: u64) {
        self.store_u64(offset, value)
    }

    /// Copies `src` into the cage starting at `offset`.
    pub fn write_bytes(&mut self, offset: u32, src: &[u8]) {
        self.store_bytes(offset, src)
    }

    /// Sets `len` bytes starting at `offset` to `byte` (zeroing, poisoning
    /// freed cells).
    pub fn fill(&mut self, offset: u32, len: u32, byte: u8) {
        self.store_fill(offset, len, byte)
    }

    /// [`Cage::write_u8`] through a shared reference.
    pub(crate) fn store_u8(&self, offset: u32, value: u8) {
        let p = self.checked_ptr(offset, 1, 1);
        // SAFETY: the byte is inside a committed (writable) chunk. One
        // thread only (`!Sync`) and no Rust reference points into cage
        // memory, so nothing else observes or aliases it during the write.
        unsafe { p.write(value) }
    }

    /// [`Cage::write_u32`] through a shared reference.
    pub(crate) fn store_u32(&self, offset: u32, value: u32) {
        let p = self.checked_ptr(offset, 4, 4);
        // SAFETY: four committed bytes, aligned for `u32`; no racing or
        // aliasing access, as in `store_u8`.
        unsafe { p.cast::<u32>().write(value) }
    }

    /// [`Cage::write_u64`] through a shared reference.
    pub(crate) fn store_u64(&self, offset: u32, value: u64) {
        let p = self.checked_ptr(offset, 8, 8);
        // SAFETY: eight committed bytes, aligned for `u64`; no racing or
        // aliasing access, as in `store_u8`.
        unsafe { p.cast::<u64>().write(value) }
    }

    /// [`Cage::write_bytes`] through a shared reference.
    pub(crate) fn store_bytes(&self, offset: u32, src: &[u8]) {
        if src.is_empty() {
            return;
        }
        let len = u32::try_from(src.len()).expect("write larger than the cage");
        let p = self.checked_ptr(offset, len, 1);
        // SAFETY: `len` committed, writable bytes start at `p`; `src` is a
        // Rust buffer, and safe code never borrows cage memory, so `src`
        // is outside the cage and the ranges don't overlap. No racing
        // access, as in `store_u8`.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), p, src.len()) }
    }

    /// [`Cage::fill`] through a shared reference.
    pub(crate) fn store_fill(&self, offset: u32, len: u32, byte: u8) {
        if len == 0 {
            return;
        }
        let p = self.checked_ptr(offset, len, 1);
        // SAFETY: `len` committed, writable bytes start at `p`; no racing
        // or aliasing access, as in `store_u8`.
        unsafe { std::ptr::write_bytes(p, byte, len as usize) }
    }

    /// Copies `dst.len()` bytes starting at `offset` out of the cage.
    pub fn read_bytes(&self, offset: u32, dst: &mut [u8]) {
        if dst.is_empty() {
            return;
        }
        let len = u32::try_from(dst.len()).expect("read larger than the cage");
        let p = self.checked_ptr(offset, len, 1);
        // SAFETY: `len` committed bytes start at `p`; `dst` is a distinct
        // Rust buffer (it cannot point into the cage, which safe code never
        // borrows), so the ranges don't overlap.
        unsafe { std::ptr::copy_nonoverlapping(p, dst.as_mut_ptr(), dst.len()) }
    }
}

impl Drop for Cage {
    fn drop(&mut self) {
        let len = Self::reservation_len(self.size).expect("length was valid at reserve");
        // SAFETY: `base` and `len` are exactly the reservation made in
        // `new`, released once here; no pointer into it outlives `self`
        // because safe code only ever gets copies of cage contents.
        unsafe { sys::release(self.base.as_ptr(), len) }
    }
}

impl fmt::Debug for Cage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cage")
            .field("size", &self.size)
            .field("committed_chunks", &self.committed_chunks)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
