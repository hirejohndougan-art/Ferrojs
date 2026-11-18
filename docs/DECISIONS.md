# Decision log

The decision log for FerroJS.

Every major choice, what was rejected and why. Read this at the start of every
work session. Newest decisions are added at the bottom; a changed decision gets
a new entry that supersedes the old one rather than an edit.

Status: **Proposed** until John confirms, then **Accepted**.

---

## D-001 Value representation: NaN-boxing
- **Status:** Proposed (2026-11-18)
- **Choice:** every value is 64 bits. Doubles are stored as-is, with all NaNs
  folded into one canonical NaN. Other kinds live in the top of the negative
  NaN space (tags 0xFFF9 to 0xFFFF); heap references carry a 32-bit offset.
- **Rejected:** pointer tagging (V8 style). It boxes doubles on the heap,
  costing allocations in numeric code.
- **Why:** no allocation for doubles; used by JavaScriptCore and SpiderMonkey;
  fits 32-bit heap offsets (D-002) with room to spare.
- **Implemented:** `crates/ferrojs-value`.

## D-002 Heap and references: 4 GB cage, 32-bit offsets
- **Status:** Proposed (2026-11-18)
- **Choice:** each engine instance reserves one heap region (4 GB by default).
  References are 32-bit offsets into it, never raw pointers.
- **Rejected:** index-based heap (extra indirection on every access); raw
  64-bit pointers (no sandbox, bigger objects).
- **Why:** sandbox, smaller objects and speed from one design. Same trade-off
  as V8's pointer compression. Large-heap mode can come later.

## D-003 Optimizing tier: Warp-style, fed by shared inline caches
- **Status:** Proposed (2026-11-18)
- **Choice:** each bytecode op is defined once and generated into every tier.
  Inline caches record fast paths in one shared format (SpiderMonkey's CacheIR
  model) that feeds an SSA optimizer.
- **Rejected:** Truffle-style partial evaluation (heavy, hard to debug);
  separate type inference (SpiderMonkey removed theirs for Warp).

## D-004 Unsafe code boundary
- **Status:** Proposed (2026-11-18)
- **Choice:** `unsafe` is allowed only in `ferrojs-core`. The workspace forbids it
  everywhere else (`unsafe_code = "forbid"` in the root `Cargo.toml`). Every
  `unsafe` block carries a `// SAFETY:` comment.
- **Why:** keeps the code that needs auditing small and easy to find.

## D-005 Value equality in Rust is bit equality
- **Status:** Proposed (2026-11-18)
- **Choice:** `Value`'s `PartialEq` compares encodings only. JavaScript `==`,
  `===` and SameValue are runtime functions, never Rust operators.
- **Why:** prevents a whole class of subtle bugs where Rust code silently uses
  the wrong JS equality (e.g. NaN and -0 cases).

## D-006 Untrusted bits are decoded with a checked constructor
- **Status:** Proposed (2026-11-18)
- **Choice:** values read from cache files or any outside source go through
  `Value::from_bits_checked`, which rejects encodings the engine never produces.
- **Why:** supports the plan's rule that cache files are untrusted input.

---

The full set of planned decisions (GC, frames, tail calls, baseline tier,
warm starts, concurrency, platforms, parser, accuracy scope) is in the
planning document and gets logged here as each is implemented.
