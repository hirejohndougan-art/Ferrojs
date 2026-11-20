//! Bump allocation in the cage (D-022, D-027).
//!
//! This is the Phase 1 allocator before the collector exists: cells are
//! bumped out of 256 KB chunks, and cells bigger than [`LARGE_CELL`] get a
//! run of chunks of their own. Nothing is freed yet; task 1.3 adds marking,
//! sweeping, size classes and free lists on top of the same chunk layout.
//!
//! Bump chunks mix cells of every size for now (D-027); 1.3 replaces them
//! with one chunk per size class, as D-022 describes.
//!
//! No `unsafe` here: all memory access goes through the checked [`Cage`]
//! accessors.

use std::fmt;

use crate::cage::{Cage, CageError, CHUNK_SIZE};

/// Every cell offset and size is a multiple of this.
pub const CELL_ALIGN: u32 = 8;

/// Bytes at the start of every chunk (or chunk run) reserved for the chunk
/// header: one header word, padding to 64 bytes, then the side mark bitmap
/// (one bit per 8-byte granule of a 256 KB chunk, 4 KB) that the collector
/// fills in from task 1.3. Cells start after it.
pub const CHUNK_HEADER_SIZE: u32 = 64 + CHUNK_SIZE / CELL_ALIGN / 8;

/// Cells larger than this get their own chunk run (D-022: size classes go
/// up to 8 KB). A large run's header still reserves a full mark bitmap,
/// though one bit would do; it keeps every chunk's layout the same.
pub const LARGE_CELL: u32 = 8 * 1024;

/// What a chunk holds; the low byte of the chunk header word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ChunkKind {
    /// Small cells bumped one after another.
    Bump = 1,
    /// The first chunk of a run holding one large cell. The header word's
    /// upper 32 bits hold the run length in chunks.
    LargeRun = 2,
}

impl ChunkKind {
    /// Decodes a chunk header word's kind byte.
    pub fn from_header(word: u64) -> Option<ChunkKind> {
        match word as u8 {
            1 => Some(ChunkKind::Bump),
            2 => Some(ChunkKind::LargeRun),
            _ => None,
        }
    }
}

/// The allocation could not be satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocError {
    /// Size 0, or larger than any cage can hold.
    BadSize(u64),
    /// The cage has no room left for a cell of this size.
    OutOfMemory { size: u32 },
    /// The system refused to commit memory.
    Cage(CageError),
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AllocError::BadSize(s) => write!(f, "bad cell size {s}"),
            AllocError::OutOfMemory { size } => {
                write!(f, "out of memory allocating {size} bytes")
            }
            AllocError::Cage(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AllocError {}

/// A cage plus a bump allocator over its chunks.
#[derive(Debug)]
pub struct Heap {
    cage: Cage,
    /// Next never-used chunk index. Chunk 0 is never used.
    next_chunk: u32,
    /// Bump pointer and exclusive limit in the current bump chunk, as
    /// byte offsets (u64 so the end of a 4 GB cage is representable).
    /// Both are 0 when there is no current chunk.
    cursor: u64,
    limit: u64,
    /// Bytes handed out in cells (rounded sizes).
    allocated: u64,
}

impl Heap {
    /// A heap over a freshly reserved cage of `size` bytes.
    pub fn new(size: u64) -> Result<Heap, CageError> {
        Ok(Heap::from_cage(Cage::new(size)?))
    }

    /// A heap over a cage of the default size.
    pub fn with_default_size() -> Result<Heap, CageError> {
        Ok(Heap::from_cage(Cage::with_default_size()?))
    }

    /// A heap over `cage`, which must have no committed chunks yet.
    pub fn from_cage(cage: Cage) -> Heap {
        assert_eq!(cage.committed_chunks(), 0, "the cage is already in use");
        Heap {
            cage,
            next_chunk: 1,
            cursor: 0,
            limit: 0,
            allocated: 0,
        }
    }

    /// The cage, for reading cells.
    pub fn cage(&self) -> &Cage {
        &self.cage
    }

    // Writes go through the heap rather than a `&mut Cage`, so callers can
    // neither commit chunks behind the allocator's back nor swap its cage.
    // Callers write only into cells they allocated; the collector (1.3)
    // will own that rule.

    /// Writes one byte into a cell.
    pub fn write_u8(&mut self, offset: u32, value: u8) {
        self.cage.write_u8(offset, value)
    }

    /// Writes a `u32` into a cell at a 4-byte-aligned offset.
    pub fn write_u32(&mut self, offset: u32, value: u32) {
        self.cage.write_u32(offset, value)
    }

    /// Writes a `u64` into a cell at an 8-byte-aligned offset.
    pub fn write_u64(&mut self, offset: u32, value: u64) {
        self.cage.write_u64(offset, value)
    }

    /// Copies `src` into a cell.
    pub fn write_bytes(&mut self, offset: u32, src: &[u8]) {
        self.cage.write_bytes(offset, src)
    }

    /// Sets `len` bytes of a cell to `byte`.
    pub fn fill(&mut self, offset: u32, len: u32, byte: u8) {
        self.cage.fill(offset, len, byte)
    }

    /// Bytes handed out in cells so far.
    pub fn allocated_bytes(&self) -> u64 {
        self.allocated
    }

    /// Allocates a cell of at least `size` bytes and returns its offset:
    /// non-zero, 8-byte aligned, at or above the 64 KB null region, and not
    /// overlapping any other cell. The memory reads as zero.
    pub fn alloc(&mut self, size: u32) -> Result<u32, AllocError> {
        if size == 0 {
            return Err(AllocError::BadSize(0));
        }
        let rounded = (size as u64).next_multiple_of(CELL_ALIGN as u64);
        if rounded > self.cage.size() {
            return Err(AllocError::BadSize(size as u64));
        }
        // Rounding can reach 2^32, so sizes stay u64 from here on.
        let offset = if rounded > LARGE_CELL as u64 {
            self.alloc_large(rounded, size)?
        } else {
            self.alloc_small(rounded, size)?
        };
        self.allocated += rounded;
        Ok(offset)
    }

    fn alloc_small(&mut self, rounded: u64, size: u32) -> Result<u32, AllocError> {
        if self.cursor + rounded > self.limit {
            let chunk = self.take_chunks(1, size)?;
            let start = chunk as u64 * CHUNK_SIZE as u64;
            self.cage.write_u64(start as u32, ChunkKind::Bump as u64);
            self.cursor = start + CHUNK_HEADER_SIZE as u64;
            self.limit = start + CHUNK_SIZE as u64;
        }
        let offset = self.cursor;
        self.cursor += rounded;
        Ok(offset as u32)
    }

    fn alloc_large(&mut self, rounded: u64, size: u32) -> Result<u32, AllocError> {
        let count = (CHUNK_HEADER_SIZE as u64 + rounded).div_ceil(CHUNK_SIZE as u64);
        let count = u32::try_from(count).map_err(|_| AllocError::BadSize(rounded))?;
        let chunk = self.take_chunks(count, size)?;
        let start = (chunk as u64 * CHUNK_SIZE as u64) as u32;
        self.cage
            .write_u64(start, ChunkKind::LargeRun as u64 | (count as u64) << 32);
        Ok(start + CHUNK_HEADER_SIZE)
    }

    /// Commits the next `count` unused chunks and returns the first index.
    /// `size` is the requested cell size, for the error.
    fn take_chunks(&mut self, count: u32, size: u32) -> Result<u32, AllocError> {
        let first = self.next_chunk;
        let end = first as u64 + count as u64;
        if end > self.cage.chunk_count() as u64 {
            return Err(AllocError::OutOfMemory { size });
        }
        self.cage
            .commit_chunks(first, count)
            .map_err(AllocError::Cage)?;
        self.next_chunk = end as u32;
        Ok(first)
    }
}

#[cfg(test)]
mod tests;
