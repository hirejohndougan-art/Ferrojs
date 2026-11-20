//! Cell allocation and the Phase 1 mark-sweep collector (D-022, D-023,
//! D-027, D-028).
//!
//! Small cells (up to [`LARGE_CELL`] bytes) live in 256 KB chunks that each
//! hold one size class; a chunk is bump-allocated until full, and after a
//! collection its dead cells go on the class's free list. Bigger cells get
//! a run of chunks of their own. Mark bits live in a side bitmap in each
//! chunk's header. Marking uses an explicit stack, never recursion.
//!
//! The collector doesn't know cell layouts: the caller passes a [`Trace`]
//! implementation that lists each cell's outgoing references, and the
//! roots. Task 1.3b builds the branded handles and the engine context on
//! top of this.
//!
//! No `unsafe` here: all memory access goes through the checked [`Cage`]
//! accessors. Everything the collector trusts (which chunks hold what, the
//! free lists) is kept in this Rust struct, outside the cage.

use std::collections::BTreeSet;
use std::fmt;

use crate::cage::{Cage, CageError, CHUNK_SIZE};

/// Every cell offset and size is a multiple of this.
pub const CELL_ALIGN: u32 = 8;

/// Offset of the mark bitmap inside a chunk header.
const BITMAP_OFFSET: u32 = 64;

/// Bytes of mark bitmap per chunk: one bit per 8-byte granule.
const BITMAP_BYTES: u32 = CHUNK_SIZE / CELL_ALIGN / 8;

/// Bytes at the start of every chunk (or chunk run) reserved for the chunk
/// header: one header word, padding to 64 bytes, then the side mark bitmap
/// (4 KB). Cells start after it. A large run's header keeps a full bitmap
/// though one bit would do, so every chunk has the same layout.
pub const CHUNK_HEADER_SIZE: u32 = BITMAP_OFFSET + BITMAP_BYTES;

/// Cells larger than this get their own chunk run (D-022).
pub const LARGE_CELL: u32 = 8 * 1024;

/// Small-cell size classes, about 1.5x apart (D-022).
pub const SIZE_CLASSES: [u32; 19] = [
    16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192,
];

/// Size class index for each rounded size / 8, up to `LARGE_CELL`.
const CLASS_OF: [u8; (LARGE_CELL / CELL_ALIGN) as usize + 1] = {
    let mut table = [0u8; (LARGE_CELL / CELL_ALIGN) as usize + 1];
    let mut i = 0;
    let mut class = 0;
    while i < table.len() {
        while SIZE_CLASSES[class] < (i as u32) * CELL_ALIGN {
            class += 1;
        }
        table[i] = class as u8;
        i += 1;
    }
    table
};

/// The kind byte (a cell's first byte, D-022) of a freed cell. Cell
/// layouts defined elsewhere must never use it.
pub const FREED_KIND: u8 = 0xff;

/// The byte freed cells are filled with when poisoning is on.
pub const POISON_BYTE: u8 = 0xdb;

/// A collection is due once this many bytes were allocated since the last
/// one, or as many as survived it, whichever is more.
pub const MIN_GC_THRESHOLD: u64 = 1 << 20;

/// What a chunk holds; the low byte of the chunk header word in the cage.
/// Informational (snapshots, debugging): the collector trusts its own
/// Rust-side table instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ChunkKind {
    /// Small cells of one size class; the next header byte is the class.
    Small = 1,
    /// The first chunk of a run holding one large cell. The header word's
    /// upper 32 bits hold the run length in chunks.
    LargeRun = 2,
}

impl ChunkKind {
    /// Decodes a chunk header word's kind byte.
    pub fn from_header(word: u64) -> Option<ChunkKind> {
        match word as u8 {
            1 => Some(ChunkKind::Small),
            2 => Some(ChunkKind::LargeRun),
            _ => None,
        }
    }
}

/// The collector's own record of a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkInfo {
    /// Never committed (or chunk 0).
    Unused,
    /// Committed, holds nothing, in `free_chunks`.
    Free,
    /// Small cells of class `class`; bytes `CHUNK_HEADER_SIZE..used` of the
    /// chunk have been handed out as cells.
    Small { class: u8, used: u32 },
    /// First chunk of a large run of `count` chunks; the cell is `size`
    /// bytes at `CHUNK_HEADER_SIZE`.
    LargeHead { count: u32, size: u32 },
    /// A later chunk of the large run whose first chunk is `head`.
    LargeTail { head: u32 },
}

/// The allocation could not be satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocError {
    /// Size 0, or larger than this heap's whole cage.
    BadSize(u64),
    /// The cage has no room left for a cell of this (requested) size.
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

/// Lists the references held by cells. Implemented by the layer that
/// defines cell kinds (`ferrojs-vm`'s `cells!` table, ARCHITECTURE §6.4).
pub trait Trace {
    /// Pushes onto `edges` the offset of every cell that the cell at `cell`
    /// (with slot size `size`) references. Offset 0 (null) is ignored.
    /// Every other offset must be the start of a live cell, or the
    /// collector panics.
    fn trace(&self, cage: &Cage, cell: u32, size: u32, edges: &mut Vec<u32>);
}

/// Debugging switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapOptions {
    /// Fill freed cells with [`POISON_BYTE`] (their kind byte becomes
    /// [`FREED_KIND`] either way).
    pub poison: bool,
    /// Run the heap verifier after every collection.
    pub verify: bool,
}

impl Default for HeapOptions {
    /// Both on in debug builds, off in release builds.
    fn default() -> HeapOptions {
        HeapOptions {
            poison: cfg!(debug_assertions),
            verify: cfg!(debug_assertions),
        }
    }
}

/// What a collection did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CollectStats {
    /// Bytes in cells that survived.
    pub live_bytes: u64,
    /// Bytes in cells freed by this collection.
    pub freed_bytes: u64,
    /// Chunks given back to the free-chunk pool.
    pub freed_chunks: u32,
}

/// A cage, the allocator over its chunks, and the collector.
#[derive(Debug)]
pub struct Heap {
    cage: Cage,
    options: HeapOptions,
    /// One entry per chunk of the cage.
    chunks: Vec<ChunkInfo>,
    /// Next never-committed chunk index. Chunk 0 is never used.
    next_chunk: u32,
    /// Committed chunks holding nothing, reused before fresh ones.
    free_chunks: BTreeSet<u32>,
    /// Per size class: the chunk being bump-allocated (0 = none), and how
    /// many of its bytes are used. For that chunk, `bump_used` is current
    /// and its `ChunkInfo::Small::used` is stale (see `small_used`).
    bump_chunk: [u32; SIZE_CLASSES.len()],
    bump_used: [u32; SIZE_CLASSES.len()],
    /// Per size class: free cells, rebuilt by every sweep.
    free_cells: Vec<Vec<u32>>,
    /// Bytes in cells (slot sizes) that are allocated and not yet freed.
    allocated: u64,
    /// Bytes allocated since the last collection.
    since_gc: u64,
    /// `since_gc` at which the next collection is due.
    threshold: u64,
    /// Reused between collections.
    mark_stack: Vec<u32>,
    edges: Vec<u32>,
    /// Set while `collect` runs. Still set at the next `collect` means the
    /// last one panicked part-way, leaving mark bits behind.
    collecting: bool,
}

impl Heap {
    /// A heap over a freshly reserved cage of `size` bytes.
    pub fn new(size: u64) -> Result<Heap, CageError> {
        Ok(Heap::from_cage(Cage::new(size)?, HeapOptions::default()))
    }

    /// A heap over a cage of the default size.
    pub fn with_default_size() -> Result<Heap, CageError> {
        Ok(Heap::from_cage(
            Cage::with_default_size()?,
            HeapOptions::default(),
        ))
    }

    /// A heap over `cage`, which must have no committed chunks yet.
    pub fn from_cage(cage: Cage, options: HeapOptions) -> Heap {
        assert_eq!(cage.committed_chunks(), 0, "the cage is already in use");
        let chunk_count = cage.chunk_count() as usize;
        Heap {
            cage,
            options,
            chunks: vec![ChunkInfo::Unused; chunk_count],
            next_chunk: 1,
            free_chunks: BTreeSet::new(),
            bump_chunk: [0; SIZE_CLASSES.len()],
            bump_used: [0; SIZE_CLASSES.len()],
            free_cells: vec![Vec::new(); SIZE_CLASSES.len()],
            allocated: 0,
            since_gc: 0,
            threshold: MIN_GC_THRESHOLD,
            mark_stack: Vec::new(),
            edges: Vec::new(),
            collecting: false,
        }
    }

    /// The debugging switches in force.
    pub fn options(&self) -> HeapOptions {
        self.options
    }

    /// The cage, for reading cells.
    pub fn cage(&self) -> &Cage {
        &self.cage
    }

    // Writes go through the heap rather than a `&mut Cage`, so callers can
    // neither commit chunks behind the allocator's back nor swap its cage,
    // and every write is checked to land inside one live cell: never in a
    // chunk header (mark bits), a freed cell or unallocated memory.

    /// Panics unless `len` bytes at `offset` are cell memory: always
    /// checked is that they are outside chunk headers (mark bits) and
    /// inside handed-out memory; with `verify` on, also that they lie
    /// inside one live cell.
    #[inline]
    fn check_cell_write(&self, offset: u32, len: u32) {
        let ok = if self.options.verify {
            self.cell_containing(offset)
                .is_some_and(|(cell, size)| offset as u64 + len as u64 <= cell as u64 + size as u64)
        } else {
            self.in_cell_memory(offset, len)
        };
        assert!(
            ok,
            "heap write of {len} bytes at {offset:#x} is not inside one live cell"
        );
    }

    /// The cheap write check: the range starts and ends in handed-out cell
    /// memory of the same chunk or large run, never in a chunk header.
    #[inline]
    fn in_cell_memory(&self, offset: u32, len: u32) -> bool {
        let end = offset as u64 + len as u64;
        let chunk = offset / CHUNK_SIZE;
        let rel = offset % CHUNK_SIZE;
        match self.chunks.get(chunk as usize) {
            Some(&ChunkInfo::Small { class, used }) => {
                rel >= CHUNK_HEADER_SIZE
                    && end
                        <= (chunk * CHUNK_SIZE) as u64 + self.small_used(chunk, class, used) as u64
            }
            Some(&ChunkInfo::LargeHead { size, .. }) => {
                let cell = chunk * CHUNK_SIZE + CHUNK_HEADER_SIZE;
                offset >= cell && end <= cell as u64 + size as u64
            }
            Some(&ChunkInfo::LargeTail { head }) => match self.chunks[head as usize] {
                ChunkInfo::LargeHead { size, .. } => {
                    end <= (head * CHUNK_SIZE + CHUNK_HEADER_SIZE) as u64 + size as u64
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// The live cell containing `offset`, as (start, slot size).
    #[inline]
    fn cell_containing(&self, offset: u32) -> Option<(u32, u32)> {
        let chunk = offset / CHUNK_SIZE;
        let rel = offset % CHUNK_SIZE;
        let (head, size) = match *self.chunks.get(chunk as usize)? {
            ChunkInfo::Small { class, used } => {
                let slot = SIZE_CLASSES[class as usize];
                let used = self.small_used(chunk, class, used);
                if rel < CHUNK_HEADER_SIZE || rel >= used {
                    return None;
                }
                let cell = offset - (rel - CHUNK_HEADER_SIZE) % slot;
                return (self.cage.read_u8(cell) != FREED_KIND).then_some((cell, slot));
            }
            ChunkInfo::LargeHead { size, .. } => (chunk, size),
            ChunkInfo::LargeTail { head } => match self.chunks[head as usize] {
                ChunkInfo::LargeHead { size, .. } => (head, size),
                _ => return None,
            },
            ChunkInfo::Unused | ChunkInfo::Free => return None,
        };
        let cell = head * CHUNK_SIZE + CHUNK_HEADER_SIZE;
        (offset >= cell).then_some((cell, size))
    }

    /// Writes one byte into a live cell.
    pub fn write_u8(&mut self, offset: u32, value: u8) {
        self.check_cell_write(offset, 1);
        self.cage.write_u8(offset, value)
    }

    /// Writes a `u32` into a live cell at a 4-byte-aligned offset.
    pub fn write_u32(&mut self, offset: u32, value: u32) {
        self.check_cell_write(offset, 4);
        self.cage.write_u32(offset, value)
    }

    /// Writes a `u64` into a live cell at an 8-byte-aligned offset.
    pub fn write_u64(&mut self, offset: u32, value: u64) {
        self.check_cell_write(offset, 8);
        self.cage.write_u64(offset, value)
    }

    /// Copies `src` into a live cell.
    pub fn write_bytes(&mut self, offset: u32, src: &[u8]) {
        if src.is_empty() {
            return;
        }
        let len = u32::try_from(src.len()).expect("write larger than the cage");
        self.check_cell_write(offset, len);
        self.cage.write_bytes(offset, src)
    }

    /// Sets `len` bytes of a live cell to `byte`.
    pub fn fill(&mut self, offset: u32, len: u32, byte: u8) {
        if len == 0 {
            return;
        }
        self.check_cell_write(offset, len);
        self.cage.fill(offset, len, byte)
    }

    /// Bytes in allocated cells (slot sizes), including garbage not yet
    /// collected.
    pub fn allocated_bytes(&self) -> u64 {
        self.allocated
    }

    /// Whether enough was allocated since the last collection that the
    /// next one is due.
    pub fn wants_collection(&self) -> bool {
        self.since_gc >= self.threshold
    }

    /// Allocates a cell of at least `size` bytes and returns its offset:
    /// non-zero, 8-byte aligned, at or above the 64 KB null region, and not
    /// overlapping any other live cell. The memory reads as zero. Never
    /// collects; the caller decides when to (see [`Heap::wants_collection`]).
    pub fn alloc(&mut self, size: u32) -> Result<u32, AllocError> {
        if size == 0 {
            return Err(AllocError::BadSize(0));
        }
        let rounded = (size as u64).next_multiple_of(CELL_ALIGN as u64);
        if rounded > self.cage.size() {
            return Err(AllocError::BadSize(size as u64));
        }
        // Rounding can reach 2^32, so sizes stay u64 until checked.
        let (offset, slot) = if rounded <= LARGE_CELL as u64 {
            let class = CLASS_OF[(rounded / CELL_ALIGN as u64) as usize] as usize;
            (self.alloc_small(class, size)?, SIZE_CLASSES[class])
        } else {
            self.alloc_large(rounded, size)?
        };
        self.allocated += slot as u64;
        self.since_gc += slot as u64;
        Ok(offset)
    }

    fn alloc_small(&mut self, class: usize, size: u32) -> Result<u32, AllocError> {
        let slot = SIZE_CLASSES[class];
        if let Some(at) = self.free_cells[class].pop() {
            // Freed cells hold poison or stale data.
            self.cage.fill(at, slot, 0);
            return Ok(at);
        }
        let chunk = self.bump_chunk[class];
        let used = self.bump_used[class];
        if chunk != 0 && used + slot <= CHUNK_SIZE {
            // `used < CHUNK_SIZE`, so this stays below 2^32.
            self.bump_used[class] = used + slot;
            return Ok(chunk * CHUNK_SIZE + used);
        }
        let new_chunk = self.take_chunks(1, size)?;
        if chunk != 0 {
            // Retire the full chunk: its own record becomes current.
            self.chunks[chunk as usize] = ChunkInfo::Small {
                class: class as u8,
                used,
            };
        }
        let chunk = new_chunk;
        let start = chunk * CHUNK_SIZE;
        self.cage
            .write_u64(start, ChunkKind::Small as u64 | (class as u64) << 8);
        self.chunks[chunk as usize] = ChunkInfo::Small {
            class: class as u8,
            used: CHUNK_HEADER_SIZE + slot,
        };
        self.bump_chunk[class] = chunk;
        self.bump_used[class] = CHUNK_HEADER_SIZE + slot;
        Ok(start + CHUNK_HEADER_SIZE)
    }

    fn alloc_large(&mut self, rounded: u64, size: u32) -> Result<(u32, u32), AllocError> {
        let count = (CHUNK_HEADER_SIZE as u64 + rounded).div_ceil(CHUNK_SIZE as u64);
        if count >= self.cage.chunk_count() as u64 {
            return Err(AllocError::OutOfMemory { size });
        }
        let count = count as u32;
        let first = self.take_chunks(count, size)?;
        // The run fits in the cage beside chunk 0, so this is below 2^32.
        let rounded = rounded as u32;
        let start = first * CHUNK_SIZE;
        self.cage
            .write_u64(start, ChunkKind::LargeRun as u64 | (count as u64) << 32);
        self.chunks[first as usize] = ChunkInfo::LargeHead {
            count,
            size: rounded,
        };
        for c in first + 1..first + count {
            self.chunks[c as usize] = ChunkInfo::LargeTail { head: first };
        }
        Ok((start + CHUNK_HEADER_SIZE, rounded))
    }

    /// Finds `count` contiguous chunks, reusing free ones (zeroed again)
    /// before committing fresh ones, and returns the first index. `size`
    /// is the requested cell size, for the error.
    fn take_chunks(&mut self, count: u32, size: u32) -> Result<u32, AllocError> {
        if let Some(first) = self.find_free_run(count) {
            for c in first..first + count {
                self.free_chunks.remove(&c);
            }
            // `count < chunk_count <= 2^14`, so the length fits in u32.
            self.cage.fill(first * CHUNK_SIZE, count * CHUNK_SIZE, 0);
            return Ok(first);
        }
        // Free chunks just below `next_chunk` can start a run that fresh
        // chunks finish.
        let mut reused = 0;
        while reused < count && self.free_chunks.contains(&(self.next_chunk - reused - 1)) {
            reused += 1;
        }
        let fresh = count - reused;
        let end = self.next_chunk as u64 + fresh as u64;
        if end > self.cage.chunk_count() as u64 {
            return Err(AllocError::OutOfMemory { size });
        }
        self.cage
            .commit_chunks(self.next_chunk, fresh)
            .map_err(AllocError::Cage)?;
        let first = self.next_chunk - reused;
        for c in first..self.next_chunk {
            self.free_chunks.remove(&c);
        }
        if reused > 0 {
            self.cage.fill(first * CHUNK_SIZE, reused * CHUNK_SIZE, 0);
        }
        self.next_chunk = end as u32;
        Ok(first)
    }

    /// Bytes used in small chunk `chunk` of size class `class`, whose own
    /// record says `used`.
    fn small_used(&self, chunk: u32, class: u8, used: u32) -> u32 {
        if self.bump_chunk[class as usize] == chunk {
            self.bump_used[class as usize]
        } else {
            used
        }
    }

    /// The lowest run of `count` consecutive free chunks.
    fn find_free_run(&self, count: u32) -> Option<u32> {
        let mut run_start = 0;
        let mut run_len = 0;
        for &c in &self.free_chunks {
            if run_len > 0 && c == run_start + run_len {
                run_len += 1;
            } else {
                run_start = c;
                run_len = 1;
            }
            if run_len == count {
                return Some(run_start);
            }
        }
        None
    }

    /// If `offset` is the start of a live (allocated, not freed) cell,
    /// returns its slot size.
    pub fn cell_size(&self, offset: u32) -> Option<u32> {
        if offset == 0 || offset % CELL_ALIGN != 0 {
            return None;
        }
        let chunk = offset / CHUNK_SIZE;
        let rel = offset % CHUNK_SIZE;
        match *self.chunks.get(chunk as usize)? {
            ChunkInfo::Small { class, used } => {
                let slot = SIZE_CLASSES[class as usize];
                let used = self.small_used(chunk, class, used);
                let live = rel >= CHUNK_HEADER_SIZE
                    && rel < used
                    && (rel - CHUNK_HEADER_SIZE) % slot == 0
                    && self.cage.read_u8(offset) != FREED_KIND;
                live.then_some(slot)
            }
            ChunkInfo::LargeHead { size, .. } => (rel == CHUNK_HEADER_SIZE).then_some(size),
            ChunkInfo::Unused | ChunkInfo::Free | ChunkInfo::LargeTail { .. } => None,
        }
    }

    /// Whether `offset` is the start of a live cell.
    pub fn is_live_cell(&self, offset: u32) -> bool {
        self.cell_size(offset).is_some()
    }

    /// Where the mark bit for the cell at `offset` lives: the bitmap word's
    /// offset and the bit within it.
    fn mark_bit(offset: u32) -> (u32, u64) {
        let chunk_start = offset - offset % CHUNK_SIZE;
        let bit = (offset % CHUNK_SIZE) / CELL_ALIGN;
        (
            chunk_start + BITMAP_OFFSET + (bit / 64) * 8,
            1 << (bit % 64),
        )
    }

    fn is_marked(&self, offset: u32) -> bool {
        let (word, mask) = Self::mark_bit(offset);
        self.cage.read_u64(word) & mask != 0
    }

    /// Marks the cell at `edge` (if not null and not yet marked) and queues
    /// it for tracing. Panics if `edge` is not a live cell.
    fn mark_edge(&mut self, edge: u32) {
        if edge == 0 {
            return;
        }
        assert!(
            self.is_live_cell(edge),
            "GC reference {edge:#x} does not point at a live cell"
        );
        let (word, mask) = Self::mark_bit(edge);
        let bits = self.cage.read_u64(word);
        if bits & mask == 0 {
            self.cage.write_u64(word, bits | mask);
            self.mark_stack.push(edge);
        }
    }

    /// Runs a full stop-the-world mark-sweep collection. Every cell not
    /// reachable from `roots` through `tracer` is freed. Panics if a root
    /// or a traced reference is neither null nor a live cell.
    pub fn collect<R, T>(&mut self, roots: R, tracer: &T) -> CollectStats
    where
        R: IntoIterator<Item = u32>,
        T: Trace + ?Sized,
    {
        if self.collecting {
            self.recover_from_panic();
        }
        if self.options.verify {
            self.check_bitmaps_clear();
        }
        self.collecting = true;
        let roots: Vec<u32> = roots.into_iter().collect();
        for &root in &roots {
            self.mark_edge(root);
        }
        let mut edges = std::mem::take(&mut self.edges);
        while let Some(cell) = self.mark_stack.pop() {
            let size = self.cell_size(cell).expect("marked cells are live");
            edges.clear();
            tracer.trace(&self.cage, cell, size, &mut edges);
            for &edge in &edges {
                self.mark_edge(edge);
            }
        }
        self.edges = edges;
        let stats = self.sweep();
        self.allocated = stats.live_bytes;
        self.since_gc = 0;
        self.threshold = MIN_GC_THRESHOLD.max(stats.live_bytes);
        self.collecting = false;
        if self.options.verify {
            self.verify(roots, tracer);
        }
        stats
    }

    /// Clears what a collection that panicked part-way left behind: mark
    /// bits (which would hide reachable cells from the next mark) and the
    /// mark stack. Nothing was swept, so nothing else changed.
    fn recover_from_panic(&mut self) {
        self.mark_stack.clear();
        for chunk in 1..self.next_chunk {
            if let Some(len) = self.bitmap_in_use(chunk) {
                self.cage.fill(chunk * CHUNK_SIZE + BITMAP_OFFSET, len, 0);
            }
        }
        self.collecting = false;
    }

    /// Panics if any chunk in use has a mark bit set outside a collection.
    fn check_bitmaps_clear(&self) {
        for chunk in 1..self.next_chunk {
            if let Some(len) = self.bitmap_in_use(chunk) {
                let words = chunk * CHUNK_SIZE + BITMAP_OFFSET;
                for i in 0..len / 8 {
                    assert_eq!(
                        self.cage.read_u64(words + 8 * i),
                        0,
                        "heap verifier: chunk {chunk} has mark bits left set"
                    );
                }
            }
        }
    }

    /// Bytes at the start of a chunk's mark bitmap that can hold set bits:
    /// those covering the handed-out cells (mark bits are only ever set at
    /// cell starts). A multiple of 8. `None` for chunks without cells.
    fn bitmap_in_use(&self, chunk: u32) -> Option<u32> {
        let end = match self.chunks[chunk as usize] {
            ChunkInfo::Small { class, used } => self.small_used(chunk, class, used),
            ChunkInfo::LargeHead { .. } => CHUNK_HEADER_SIZE + CELL_ALIGN,
            ChunkInfo::Unused | ChunkInfo::Free | ChunkInfo::LargeTail { .. } => return None,
        };
        // One bit per 8-byte granule below `end`, rounded up to whole words.
        Some((end / CELL_ALIGN).div_ceil(64) * 8)
    }

    fn sweep(&mut self) -> CollectStats {
        let mut stats = CollectStats::default();
        for list in &mut self.free_cells {
            list.clear();
        }
        let mut dead = Vec::new();
        for chunk in 1..self.next_chunk {
            let start = chunk * CHUNK_SIZE;
            match self.chunks[chunk as usize] {
                ChunkInfo::Small { class, used } => {
                    let slot = SIZE_CLASSES[class as usize];
                    let used = self.small_used(chunk, class, used);
                    dead.clear();
                    let mut live = 0u32;
                    // Relative offsets: `start + used` can be 2^32.
                    let mut rel = CHUNK_HEADER_SIZE;
                    while rel < used {
                        let at = start + rel;
                        if self.is_marked(at) {
                            live += 1;
                        } else {
                            if self.cage.read_u8(at) != FREED_KIND {
                                self.free_cell(at, slot);
                                stats.freed_bytes += slot as u64;
                            }
                            dead.push(at);
                        }
                        rel += slot;
                    }
                    stats.live_bytes += live as u64 * slot as u64;
                    let len = self.bitmap_in_use(chunk).expect("a small chunk");
                    self.cage.fill(start + BITMAP_OFFSET, len, 0);
                    if live == 0 && self.bump_chunk[class as usize] != chunk {
                        self.chunks[chunk as usize] = ChunkInfo::Free;
                        self.free_chunks.insert(chunk);
                        stats.freed_chunks += 1;
                    } else {
                        self.free_cells[class as usize].extend_from_slice(&dead);
                    }
                }
                ChunkInfo::LargeHead { count, size } => {
                    let cell = start + CHUNK_HEADER_SIZE;
                    if self.is_marked(cell) {
                        stats.live_bytes += size as u64;
                        let (word, _) = Self::mark_bit(cell);
                        self.cage.write_u64(word, 0);
                    } else {
                        self.free_cell(cell, size);
                        stats.freed_bytes += size as u64;
                        for c in chunk..chunk + count {
                            self.chunks[c as usize] = ChunkInfo::Free;
                            self.free_chunks.insert(c);
                        }
                        stats.freed_chunks += count;
                    }
                }
                ChunkInfo::Unused | ChunkInfo::Free | ChunkInfo::LargeTail { .. } => {}
            }
        }
        stats
    }

    /// Poisons (if enabled) and marks a cell freed, so a stale reference to
    /// it fails at its next use instead of reading old data.
    fn free_cell(&mut self, at: u32, size: u32) {
        if self.options.poison {
            self.cage.fill(at, size, POISON_BYTE);
        }
        self.cage.write_u8(at, FREED_KIND);
    }

    /// Checks the heap and panics on the first problem:
    ///
    /// - every root, and every reference held by a live cell, is null or
    ///   points at a live cell;
    /// - no mark bits are left set;
    /// - the chunk table agrees with itself: free chunks are exactly the
    ///   pool, every large run's later chunks are its tails, and each size
    ///   class's bump chunk is a small chunk of that class;
    /// - every free-list entry is a distinct freed slot of a small chunk of
    ///   its class;
    /// - the allocated-byte count equals the live cells' slot sizes.
    pub fn verify<R, T>(&mut self, roots: R, tracer: &T)
    where
        R: IntoIterator<Item = u32>,
        T: Trace + ?Sized,
    {
        for root in roots {
            assert!(
                root == 0 || self.is_live_cell(root),
                "heap verifier: root {root:#x} is not a live cell"
            );
        }
        self.check_bitmaps_clear();
        self.check_tables();
        let mut edges = std::mem::take(&mut self.edges);
        let mut live_bytes = 0u64;
        for chunk in 1..self.next_chunk {
            let start = chunk * CHUNK_SIZE;
            let (slot, end) = match self.chunks[chunk as usize] {
                ChunkInfo::Small { class, used } => (
                    SIZE_CLASSES[class as usize],
                    self.small_used(chunk, class, used),
                ),
                ChunkInfo::LargeHead { size, .. } => (size, CHUNK_HEADER_SIZE + 1),
                ChunkInfo::Unused | ChunkInfo::Free | ChunkInfo::LargeTail { .. } => continue,
            };
            let mut rel = CHUNK_HEADER_SIZE;
            while rel < end {
                let cell = start + rel;
                if self.is_live_cell(cell) {
                    live_bytes += slot as u64;
                    edges.clear();
                    tracer.trace(&self.cage, cell, slot, &mut edges);
                    for &edge in &edges {
                        assert!(
                            edge == 0 || self.is_live_cell(edge),
                            "heap verifier: cell {cell:#x} references {edge:#x}, which is not a live cell"
                        );
                    }
                }
                // For a large run this ends the loop after its one cell.
                rel = rel.saturating_add(slot);
            }
        }
        self.edges = edges;
        assert_eq!(
            live_bytes, self.allocated,
            "heap verifier: allocated-byte count is off"
        );
    }

    /// The chunk-table, pool and free-list part of [`Heap::verify`].
    fn check_tables(&self) {
        let mut chunk = 1;
        while chunk < self.next_chunk {
            let info = self.chunks[chunk as usize];
            assert_eq!(
                info == ChunkInfo::Free,
                self.free_chunks.contains(&chunk),
                "heap verifier: chunk {chunk} and the free-chunk pool disagree"
            );
            match info {
                ChunkInfo::Unused => {
                    panic!("heap verifier: chunk {chunk} below next_chunk is unused")
                }
                ChunkInfo::LargeTail { .. } => {
                    panic!("heap verifier: chunk {chunk} is a tail without a head")
                }
                ChunkInfo::LargeHead { count, .. } => {
                    for c in chunk + 1..chunk + count {
                        assert_eq!(
                            self.chunks[c as usize],
                            ChunkInfo::LargeTail { head: chunk },
                            "heap verifier: chunk {c} is not a tail of run {chunk}"
                        );
                    }
                    chunk += count;
                    continue;
                }
                ChunkInfo::Free | ChunkInfo::Small { .. } => {}
            }
            chunk += 1;
        }
        assert!(
            self.free_chunks.iter().all(|&c| c < self.next_chunk),
            "heap verifier: a pooled chunk was never committed"
        );
        for (class, &bump) in self.bump_chunk.iter().enumerate() {
            if bump != 0 {
                assert!(
                    matches!(self.chunks[bump as usize], ChunkInfo::Small { class: c, .. } if c as usize == class),
                    "heap verifier: bump chunk {bump} of class {class} is not one of its chunks"
                );
            }
        }
        let mut seen = BTreeSet::new();
        for (class, list) in self.free_cells.iter().enumerate() {
            let slot = SIZE_CLASSES[class];
            for &cell in list {
                let chunk = cell / CHUNK_SIZE;
                let rel = cell % CHUNK_SIZE;
                let ok = match self.chunks.get(chunk as usize) {
                    Some(&ChunkInfo::Small { class: c, used }) => {
                        c as usize == class
                            && rel >= CHUNK_HEADER_SIZE
                            && rel < self.small_used(chunk, c, used)
                            && (rel - CHUNK_HEADER_SIZE) % slot == 0
                            && self.cage.read_u8(cell) == FREED_KIND
                    }
                    _ => false,
                };
                assert!(
                    ok,
                    "heap verifier: bad free-list entry {cell:#x} in class {class}"
                );
                assert!(
                    seen.insert(cell),
                    "heap verifier: {cell:#x} is on a free list twice"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
