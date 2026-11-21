# Decision log

The decision log for FerroJS.

Every major choice, what was rejected and why. Read this at the start of every
work session. Newest decisions are added at the bottom; a changed decision gets
a new entry that supersedes the old one rather than an edit.

Status: since D-012 (2026-11-19) development is fully automated, so each
autopilot run accepts the decisions it makes: **Accepted (autopilot, date)**.

---

## D-001 Value representation: NaN-boxing
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** every value is 64 bits. Doubles are stored as-is, with all NaNs
  folded into one canonical NaN. Other kinds live in the top of the negative
  NaN space (tags 0xFFF9 to 0xFFFF); heap references carry a 32-bit offset.
- **Rejected:** pointer tagging (V8 style). It boxes doubles on the heap,
  costing allocations in numeric code.
- **Why:** no allocation for doubles; used by JavaScriptCore and SpiderMonkey;
  fits 32-bit heap offsets (D-002) with room to spare.
- **Implemented:** `crates/ferrojs-value`.

## D-002 Heap and references: 4 GB cage, 32-bit offsets
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** each engine instance reserves one heap region (4 GB by default).
  References are 32-bit offsets into it, never raw pointers.
- **Rejected:** index-based heap (extra indirection on every access); raw
  64-bit pointers (no sandbox, bigger objects).
- **Why:** sandbox, smaller objects and speed from one design. Same trade-off
  as V8's pointer compression. Large-heap mode can come later.

## D-003 Optimizing tier: Warp-style, fed by shared inline caches
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** each bytecode op is defined once and generated into every tier.
  Inline caches record fast paths in one shared format (SpiderMonkey's CacheIR
  model) that feeds an SSA optimizer.
- **Rejected:** Truffle-style partial evaluation (heavy, hard to debug);
  separate type inference (SpiderMonkey removed theirs for Warp).

## D-004 Unsafe code boundary
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** `unsafe` is allowed only in `ferrojs-core`. The workspace forbids it
  everywhere else (`unsafe_code = "forbid"` in the root `Cargo.toml`). Every
  `unsafe` block carries a `// SAFETY:` comment.
- **Why:** keeps the code that needs auditing small and easy to find.

## D-005 Value equality in Rust is bit equality
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** `Value`'s `PartialEq` compares encodings only. JavaScript `==`,
  `===` and SameValue are runtime functions, never Rust operators.
- **Why:** prevents a whole class of subtle bugs where Rust code silently uses
  the wrong JS equality (e.g. NaN and -0 cases).

## D-006 Untrusted bits are decoded with a checked constructor
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** values read from cache files or any outside source go through
  `Value::from_bits_checked`, which rejects encodings the engine never produces.
- **Why:** supports the plan's rule that cache files are untrusted input.

## D-007 Interrupt checks in every tier
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** the interpreter loop and every JIT loop back-edge and function
  entry check an interrupt flag.
- **Why:** CPU time limits, script termination and the debugger must work in
  every tier. Retrofitting this into JIT code later is painful.

## D-008 All nondeterminism goes through host hooks
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** `Math.random`, clocks (`Date.now`, `performance.now`) and job
  ordering are supplied by the embedder through hooks, with sensible defaults.
- **Why:** makes deterministic mode a configuration, not a retrofit, and keeps
  tests reproducible.

## D-009 Snapshot-friendly heap
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** nothing in the heap stores raw machine addresses; references are
  cage offsets (D-002), and native function pointers are stored as indexes into
  a registered table.
- **Why:** a saved heap can be restored in milliseconds without rewriting
  pointers, enabling startup snapshots.

## D-010 WebAssembly compiled by Cranelift
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** Wasm modules are validated with `wasmparser` and compiled with
  Cranelift (the Wasmtime pipeline). FerroJS owns the JS API and Wasm memory.
- **Why:** proven in production; Wasm's fixed types make a fast compiler
  enough; stays 100% Rust.

## D-011 Test262 runner design
- **Status:** Accepted (2026-11-19, under D-012)
- **Choice:** our own runner (`crates/ferrojs-test262`) that follows Test262's
  INTERPRETING.md exactly. Engines only report what happened (error, phase,
  printed output); one shared judge decides pass or fail. Test262 is pinned to
  a commit (`tools/test262-commit.txt`) and a baseline of passing runs
  (`conformance/test262-baseline.txt`) gates every change: a test that passed
  before must keep passing.
- **Host settings:** the main agent can block (`CanBlock = true`, like Node and
  shells), so the 2 `CanBlockIsFalse` tests are skipped. `intl402` runs but is
  reported separately, matching the accuracy scope.
- **Reference engine:** Node.js runs through the same runner via a small driver
  (fresh realm per test, phases reported separately). It proves the runner is
  correct and is the first engine for differential testing.
- **Rejected:** the npm `test262-harness` tool (adds a Node dependency to the
  core gate and doesn't report phases from our engine); unpinned Test262 (new
  upstream tests would change our numbers by surprise).

## D-012 Fully autonomous development
- **Status:** Accepted (2026-11-19, chosen by John)
- **Choice:** FerroJS is built by scheduled Claude sessions about three times
  a day, with no human approval step. Each run follows `docs/AUTOPILOT.md`:
  one task from `docs/ROADMAP.md`, its own decisions logged here as
  Accepted, the full gate before merging, and an independent review by a
  separate Claude subagent for risky code (core, `unsafe`, GC, JIT). All
  earlier Proposed decisions are accepted under this entry. John can read
  progress in `docs/PROGRESS.md` and the weekly reports, but nothing waits
  on him.
- **Rejected:** human approval of decisions and risky code (John chose full
  automation); a local LLM doing part of the work (too weak for this
  engine on available hardware).
- **Why:** keeps the project moving continuously. Human review is replaced
  by automatic checks (Test262 baseline, differential testing, fuzzing,
  Miri, benchmarks) plus independent review and a monthly course check.

## D-013 Differential testing against Node.js
- **Status:** Accepted (autopilot, 2026-11-19)
- **Choice:** a `ferrojs-diff` crate with a `jsdiff` tool runs every `.js`
  file in `tests/diff/` on FerroJS and on Node.js, reusing the Test262
  runner's `Engine` trait and Node driver (fresh realm per file, Test262's
  one-argument `print`). Files run once as raw, non-module scripts. It
  compares the printed lines exactly and the uncaught error by constructor
  name and phase. Error messages are not compared. `Unsupported` counts as
  skipped; a crash or timeout on either side is a difference. The gate runs
  it twice: FerroJS vs Node, and Node vs a second Node process (checks the
  harness and that every corpus file is deterministic). Only files under
  `tests/diff/errors/` may end with an uncaught error; a test enforces it so
  a file that breaks early (e.g. uses a host API the realm lacks) can't
  silently test less.
- **Rejected:** stored expected-output files (they go stale and hide
  reference changes; Node is the oracle, as the plan says); comparing error
  messages (implementation-defined, would only produce noise); running the
  corpus as modules or with the Test262 harness (keeps files plain JS that
  any engine can run).
- **Why:** catches behaviour bugs Test262 doesn't assert on (output order,
  number formatting, job ordering) and gives every bug fix a cheap place
  for a regression test.

## D-014 Fuzzing with libFuzzer, smoke-run on stable Rust
- **Status:** Accepted (autopilot, 2026-11-19)
- **Choice:** fuzz targets live in a `fuzz/` crate laid out for `cargo-fuzz`
  (libfuzzer-sys, own `[workspace]`, excluded from the main one).
  `tools/fuzz.sh` builds them on **stable** Rust with the same
  SanitizerCoverage flags cargo-fuzz passes (minus the sanitizer), with
  debug assertions and overflow checks on, and an explicit `--target` so
  build scripts aren't instrumented. The gate runs `tools/fuzz.sh smoke`:
  each target 10 s from an empty corpus. Long runs use `tools/fuzz.sh run
  <target>` (persistent corpus in git-ignored `fuzz/corpus/`) or nightly
  `cargo fuzz` for AddressSanitizer. Targets check properties, not just
  "doesn't crash": the first two show `Value::from_bits_checked` accepts
  exactly the encodings the constructors produce.
- **Rejected:** requiring nightly + cargo-fuzz in the gate (not installable
  in every run environment, and a toolchain drift risk; ASan adds little
  while only safe code is fuzzed — revisit when `ferrojs-core` has
  `unsafe`); a home-made random-input loop (no coverage guidance, and
  libFuzzer's compare tracing finds the tag constants in milliseconds);
  checking a seed corpus into git (empty start is fast for now; add seeds
  once a target needs them, e.g. JS source for the parser).
- **Why:** coverage-guided fuzzing in every merge, with no extra toolchain.

## D-015 Instruction-count benchmarks with gungraun
- **Status:** Accepted (autopilot, 2026-11-19)
- **Choice:** benchmarks live in a `ferrojs-bench` crate and run under
  Valgrind's Callgrind through **gungraun** 0.20 (the renamed successor of
  `iai-callgrind`, which the plan names), pinned with `=` because the
  separately installed `gungraun-runner` must match. The metric is total
  instructions (`Ir`) per benchmark. `conformance/bench-baseline.txt`
  stores one count per benchmark plus the rustc and Valgrind versions;
  the `benchcheck` tool fails on a rise over 3% or a missing benchmark.
  Counts from another toolchain aren't comparable, so on a mismatch
  `tools/bench.sh` measures `origin/main` in a temporary worktree with the
  current toolchain and compares against that (CI always takes this path
  when its rustc differs). Benchmark inputs are generated deterministically
  (fixed-seed xorshift), never from host randomness.
- **Rejected:** wall-clock timing in the gate (noisy on shared CI; kept for
  the planned nightly timing runs); gungraun's own saved baselines in
  `target/` (not committed, so lost between runs); pinning the whole
  project's toolchain just for stable counts (the main-vs-branch fallback
  handles drift without that).
- **Why:** deterministic counts let a 3% threshold catch real regressions
  without flaky failures, from the first micro-benchmark on.

## D-016 Miri on ferrojs-core, locally or in CI
- **Status:** Accepted (autopilot, 2026-11-19)
- **Choice:** `tools/miri.sh` runs `cargo +nightly miri test -p ferrojs-core`
  with `-Zmiri-strict-provenance -Zmiri-symbolic-alignment-check`, and
  checks that a `cfg(miri)`-only canary test ran so a non-Miri run can't
  pass as one. The CI `miri` job always runs it (nightly + miri component,
  `FERROJS_MIRI_REQUIRED=1`). Autopilot environments that can't download
  nightly (the cloud sandbox blocks static.rust-lang.org) skip it only when
  `ferrojs-core` is unchanged; a core change must wait for the CI `miri`
  job on its pushed branch before merging. Code in `ferrojs-core` that
  Miri can't run (e.g. `mmap` for the 4 GB cage reservation) gets a
  `cfg(miri)` path with the same interface over a small `std::alloc`
  region, so the logic on top is still checked.
- **Rejected:** Miri over the whole workspace (everything else forbids
  `unsafe`; Miri would only slow the gate); default (permissive) provenance
  (the cage's offset-based references are exactly where int-to-pointer
  casts would creep in); skipping Miri whenever nightly is missing (would
  let unchecked `unsafe` merge).
- **Why:** catches undefined behaviour in the only crate allowed to have
  it, from the first `unsafe` block on (D-004).

## D-017 Progress dashboard regenerated by script, weekly
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** `tools/dashboard.sh` runs the full Test262 suite on FerroJS
  with `--out`, then the `dashboard` binary (in `ferrojs-test262`) turns
  that JSON report plus `conformance/bench-baseline.txt` into
  `docs/reports/dashboard.md`: totals, per top-level directory, per
  second-level directory (files directly under a top-level directory, like
  `harness/*.js`, count under it), and benchmark instruction counts. The
  page is labelled with the FerroJS revision, not a timestamp, so it only
  changes when results or code change. Weekly report runs regenerate and
  commit it; any run may. It is not a gate step.
- **Rejected:** regenerating it on every run (a second full Test262 pass
  and a noisy diff in every commit for numbers that are already in the
  baseline files); a shell/Python script parsing the terminal summary
  (fragile, untested; the Rust binary reuses the report types and the
  benchmark baseline parser and has unit tests); storing per-run history in
  the page (PROGRESS.md and the weekly reports already hold the trend).
- **Why:** gives John and the weekly report one place to see conformance
  by area and benchmark numbers, built from the same files the gate checks.

## D-018 Parser: oxc, pinned, plus our own early-error pass
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** `ferrojs-compiler` parses with `oxc_parser` and builds scopes
  and symbols with `oxc_semantic` (with its syntax-error checks on). Our
  own early-error pass runs after it and covers whatever Test262's negative
  syntax tests show oxc misses. The compiler reads the oxc AST directly; no
  AST of our own. oxc is pinned with `=` to the newest release whose
  `rust-version` the gate's toolchain meets (0.150.x needs 1.96; 0.159
  needs 1.98, newer than the sandbox's 1.97), and the workspace
  `rust-version` is raised to match. Upgrades are their own task and
  re-run the full gate.
- **Rejected:** Boa's parser (kept as the plan's fallback if oxc's
  early-error gap proves large); swc (larger API surface, fewer early
  errors); writing our own parser now (months of work the plan reserves
  for built-ins and the JIT); unpinned oxc (0.x releases break the API
  often).
- **Why:** oxc is MIT, fast, maintained, and its semantic pass already does
  scope analysis we need for register allocation (ARCHITECTURE.md §8).

## D-019 Bytecode format: register-based, variable-width operands
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** three-address register bytecode with no accumulator. One
  opcode byte; operands 1 byte by default, 2 or 4 after a `Wide` /
  `ExtraWide` prefix; little-endian. Operand kinds `Reg`, `Imm`, `Const`,
  `Ic`, `Label` (relative), `Count`, `EnvSlot`. Bytecode is an immutable
  heap cell owned by a `FunctionTemplate` (snapshot-friendly, D-009).
  Loop back-edges use `JumpLoop`, the op that checks interrupts (D-007).
  All bytecode passes a verifier before running when it comes from a
  cache, and in debug builds and tests always. Details: ARCHITECTURE.md §4.
- **Rejected:** an accumulator machine (V8 Ignition: smaller code, but
  every op has an implicit operand, which complicates copy-and-patch
  stencils and SSA building); fixed-width 32-bit instructions (Lua: simple
  decode but wasteful and caps register counts at 255 or forces awkward
  extension ops); stack bytecode (more instructions per expression, and
  the plan names register bytecode).
- **Why:** fewer instructions than a stack machine, explicit operands for
  every tier, compact common case.

## D-020 Op definitions: one declarative X-macro, handlers generic over a context
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** every op is defined once in `ferrojs-bytecode/src/ops.rs` in
  a single `macro_rules!` X-macro: name, operands with kinds, flags
  (`may_throw`, `may_gc`, `has_ic`, jump/terminator), doc, and a handler
  body generic over an `OpCtx` trait. The macro generates the opcode enum,
  encoder, decoder, disassembler, verifier table, metadata, and the
  interpreter dispatch; Phase 3 instantiates the same handler bodies as
  copy-and-patch stencils; Phase 5 adds optional `lower` clauses. Handlers
  hold only the fast path and call named runtime functions for the rest.
  The interpreter is a `loop { match }` in safe Rust for now.
- **Rejected:** a procedural macro crate (needs `syn`, slower builds,
  harder for later runs to debug; revisit only if `macro_rules!` can't
  express something, with a decision); a build-script code generator
  reading a separate op table file (the definitions would stop being
  ordinary Rust that rust-analyzer and rustc check in place); writing the
  interpreter and JIT by hand separately (tiers would drift apart).
- **Why:** the plan's "single op definitions" rule, in the simplest form
  that covers all planned tiers.

## D-021 Frames: one shared layout on an index-linked frame stack; proper tail calls
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** one frame stack per engine instance, a fixed reservation of
  64-bit slots outside the heap cage (default 1 Mi slots), frames linked by
  slot index. Layout: 8 header slots (callee, this, new.target,
  environment as Values; caller fp, return pc, return info, argc as raw
  words), then registers with parameters first, then extra arguments.
  Every register always holds a valid Value (filled with `undefined` at
  entry), so frames are scanned precisely without stack maps. Calls copy
  arguments into the new frame. Interpreter, native and baseline-JIT
  frames share the layout. Strict-mode tail calls (`TailCall` ops)
  overwrite the current frame in place, keeping its return linkage, so the
  stack never grows; elided frames are absent from `Error.stack`.
  Generators save and restore their frame's Value slots in their heap
  object. Details: ARCHITECTURE.md §3.
- **Rejected:** overlapping caller/callee register windows (LuaJIT style:
  saves a copy but makes missing/extra argument handling and in-place tail
  calls messy); reversed argument order below `fp` (V8 style: needs
  argument adaptation for under-application); using the Rust stack for JS
  frames (no proper tail calls, no generator frame copying, deep
  recursion overflows the native stack); shadow-stack trampolines for tail
  calls only in some tiers (tiers would disagree).
- **Why:** proper tail calls are the plan's named hardest accuracy item;
  a simple fixed-offset layout makes them, generators, precise scanning
  and the shared interpreter/baseline layout cheap.

## D-022 Heap layout: byte offsets, size-class chunks, side mark bitmaps
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** the 4 GB cage (D-002) is reserved `PROT_NONE` with a 64 KB
  trailing guard; the low 64 KB is never committed (offset 0 is null).
  References are 8-byte-aligned byte offsets. The cage is split into
  256 KB chunks: small-object chunks per size class (16 B up to 8 KB,
  about 1.5× steps) with bump allocation then free lists after sweeping,
  and large-object chunk runs. Mark bits are in a per-chunk side bitmap.
  Every cell starts with an 8-byte header: `CellKind` byte, flags byte,
  16 kind-specific bits, and a 32-bit Shape offset for objects. Cells
  hold no raw addresses and no Rust-owned collections. Details:
  ARCHITECTURE.md §5.
- **Rejected:** mark bits in headers (more memory touched by sweep, races
  with later concurrent marking, GC state inside snapshots); scaled
  offsets now (byte offsets are simpler while the cage is 4 GB; the free
  low 3 bits keep a 32 GB mode open); a single free-list allocator
  without size classes (fragmentation and slow allocation).
- **Why:** fast allocation now, and a layout the Phase 5 nursery and
  compactor can slot into.

## D-023 GC interface: borrow-checked unrooted handles plus scoped roots
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** collection may happen at any allocation or any call that can
  run JavaScript; such functions take `&mut Cx`. Unrooted handles
  (`Gc<'a, T>`, `JsValue<'a>`) are `Copy` and borrow a `NoGc<'a>` token
  from `Cx`, so the borrow checker rejects holding one across anything
  that may collect. May-collect functions return `#[must_use] Unbound<T>`,
  usable only through `.bind(cx.nogc())`. Values kept across a collection
  are rooted on a scoped handle stack (`Root<'s, T>`) or in a global root
  table (`Global<T>`). All reference writes into cells go through a
  write-barrier API in `ferrojs-core` (empty in Phase 1). Phase 1
  collector: precise, stop-the-world, non-moving mark-sweep, with GC
  stress (collect on every allocation), a debug heap verifier and
  poisoned freed cells. Details: ARCHITECTURE.md §6.
- **Rejected:** pure gc-arena (collection only between `mutate` calls:
  every built-in that can call user code, i.e. almost all of them via
  ToPrimitive, would have to be a resumable state machine); runtime-checked
  rooting with reference-counted roots (Boa style: rooting mistakes become
  runtime bugs, and counted roots block moving); conservative stack
  scanning (can't move objects it finds, defeats Phase 5).
- **Why:** keeps the plan's compile-time rooting goal and makes moving
  collection possible later without changing any caller. The known hole
  (an `Unbound` stored instead of bound) is caught by stress mode.

## D-024 Shapes with prototype, and CacheIR-format inline caches
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** hidden-class `Shape` cells form transition trees and carry
  the prototype, so one shape check covers layout and prototype. Objects
  have inline slots (default 4), out-of-line slots, and a separate elements
  store (packed, holey, sparse). Dictionary mode on non-last delete,
  more than 128 properties or attribute changes. Inline caches live in a
  per-function feedback vector (Uninit, Mono, Poly ≤ 4, Mega); a stub is a
  small CacheIR program plus a side array of stub fields, so stub code is
  shared and compiled once; the interpreter runs stubs with a CacheIR
  interpreter and a fast path for shape-guarded slot loads. Megamorphic
  sites use a per-engine (shape, key) cache. Details: ARCHITECTURE.md §7.
- **Rejected:** prototype outside the shape (prototype-chain ICs would
  need a second guard per level); per-tier IC formats (contradicts D-003);
  self-modifying patched IC code in the interpreter (needs executable
  memory before Phase 3).
- **Why:** follows D-003; one IC format for the interpreter, baseline JIT
  and Warp.

## D-025 Withdrawn
- **Status:** Withdrawn (2026-11-20). A goal of beating V8's peak speed was
  set and reverted the same night; the goal stays competitive with V8.

## D-026 Dependency checks and CI minutes
- **Status:** Accepted (2026-11-20, requested by John)
- **Choice:** `cargo-deny` joins the gate and CI (`deny.toml`,
  `tools/deny.sh`): MIT-compatible licenses only, no known vulnerabilities
  or yanked crates, crates only from crates.io. CI runs the build and tests
  on Linux for every push; macOS and Windows run weekly (Sunday) and on
  demand; a newer push cancels the older run on the same branch.
- **Rejected:** keeping three platforms on every push (uses GitHub Actions
  minutes fast while the repo is private, and macOS minutes count about ten
  times as much); making the repo public (John wants it private for now).
- **Why:** runs add dependencies on their own, so licenses and advisories
  must be checked automatically; and CI must not quietly stop when Actions
  minutes run out. Revisit platform frequency if the repo goes public.

## D-027 Cage memory backends and checked access
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** the cage (D-002, D-022) reserves address space with `mmap`
  (`PROT_NONE`, `MAP_NORESERVE` on Linux) through `libc` on Unix and with
  `VirtualAlloc(MEM_RESERVE)` through `windows-sys` on Windows; chunks are
  committed with `mprotect` / `MEM_COMMIT`. Under Miri, and on platforms
  with neither, the reservation is a zeroed `std::alloc` block (no guard
  pages; Miri's default cage is 8 chunks). Chunk 0 is never committed, so
  the low 64 KB null region is the whole first chunk. The facts safety
  relies on (base, size, committed-chunk bitmap) live in the Rust `Cage`
  struct, never inside the cage, and every access from safe code goes
  through checked accessors (bounds, commitment, alignment; reads `&self`,
  writes `&mut self`; `Cage` is neither `Send` nor `Sync`). The cage size
  is a parameter (4 GB default); code that skips checks must require
  `Cage::is_full_size`. Until 1.3, `Heap` bump-allocates cells of every
  size in shared chunks; cells over 8 KB get a chunk run; every chunk
  starts with a 4,160-byte header (kind word plus room for the mark
  bitmap).
- **Rejected:** `rustix` or `memmap2` (more code than three calls need;
  `memmap2` can't reserve without committing); handing out `&mut Cage`
  from `Heap` (lets callers commit chunks or swap cages behind the
  allocator); metadata in chunk headers as the source of truth (a stray
  write into the cage could then defeat the checks); a fast path for
  single-chunk accesses (measured 16% slower in the bump benchmark).
- **Why:** three system calls per platform, a sandbox whose checks can't be
  corrupted from inside, and tests that run unchanged under Miri.

## D-028 Phase 1 collector: Rust-side free lists, `Trace` callback, freed-kind byte
- **Status:** Accepted (autopilot, 2026-11-20)
- **Choice:** task 1.3 is split: 1.3a is the collector core, 1.3b the
  branded handles (D-023) on top. The collector (`ferrojs-core::heap`)
  allocates small cells from per-size-class chunks (D-022) with free
  lists kept as Rust `Vec`s, and large cells from chunk runs; empty
  chunks go to a free-chunk pool and are zeroed when reused. Mark bits are
  the side bitmap in each chunk header; marking uses an explicit stack.
  Cell layouts stay outside the core: `collect(roots, &impl Trace)` asks
  the caller for each cell's references, and any reference that isn't
  null or the start of a live cell panics. A freed cell's kind byte
  becomes `0xff` (`FREED_KIND`, reserved); `HeapOptions` turns on
  poisoning and the heap verifier after every collection (default on in
  debug builds). A collection is due after allocating max(1 MB, live
  bytes) since the last one; the heap never collects by itself, the
  caller (1.3b's `Cx`) decides. Writes through `Heap` are always kept out
  of chunk headers (mark bits) and unallocated memory; with the verifier
  on they must also land inside one live cell. A collection that panicked
  part-way is cleaned up at the next one.
  Bump allocation got slower in the instruction-count benchmark: the
  size-class lookup, the free-list check, the GC accounting and the write
  check (the baseline was reset for it; the old number came from a setup
  that warmed one size class, the new one warms four).
- **Accepted costs:** whether a small cell is freed is read from its kind
  byte in the cage, so cell layouts must never use `0xff` as a kind (1.3b's
  `cells!` table asserts it), and a stray raw write could flip a cell's
  state; writes through `Heap` are checked, and 1.3b's handles remove
  stale references, so the cheaper representation is kept for now. Free
  lists cost 4 bytes of Rust memory per free cell (a per-chunk free bitmap
  can replace them if that matters). Each size class keeps its current
  bump chunk even when empty, up to 19 chunks (4.75 MB).
- **Rejected:** intrusive free lists inside freed cells (the collector
  would trust data in the cage, against D-027, and poisoning would erase
  them); a per-chunk "used" field updated on every allocation (measured
  slower than per-class bump arrays); mark bits in a Rust-side table
  (D-022 puts them in the chunk header so a later concurrent marker and
  snapshots see one layout); one shared bump chunk for all sizes (D-022's
  size classes keep sweeping simple and fragmentation low).
- **Why:** a correct, checkable collector now, with no new `unsafe`, and
  the cell-layout knowledge left to the `cells!` table that will own it.

## D-029 Sunday marathon run
- **Status:** Accepted (2026-11-20, requested by John)
- **Choice:** besides the three nightly one-task runs, a weekly run starts
  Sunday around 10 PM Central and keeps taking tasks, one after another,
  until it runs out of usage or work. Each task still goes through the
  full procedure (claim, gate, review, CI Miri, merge, record) before the
  next starts, and the run pushes after every meaningful step because it
  can end without warning. Claims keep it from colliding with the Monday
  1 AM run.
- **Rejected:** keeping only one task per run (John wants faster progress);
  batching several tasks into one merge (harder to review and to undo).
- **Why:** more progress per week without loosening any check. It uses
  more of John's usage on Sunday nights, which he chose knowingly.

## D-030 Branded handles: writes through the shared token, roots through scopes
- **Status:** Accepted (autopilot, 2026-11-21)
- **Choice:** task 1.3b implements D-023 in `ferrojs-core::gc`. `Cx` owns
  the `Heap`, a boxed `Trace`, the handle stack and the global table.
  `Cx::alloc` and `Cx::collect` take `&mut Cx`; allocation collects first
  in stress mode (`CxOptions::gc_stress`, default from
  `FERROJS_GC_STRESS=1`), when `Heap::wants_collection`, and once more on
  out-of-memory. Reading and writing cell fields goes through the
  `NoGc<'a>` token (a shared borrow of `Cx`), so code can write while
  holding unrooted `Gc<'a, T>` handles; for that, `Heap`'s checked
  writes take `&self`, and the cage got crate-private `store_*` writes
  through `&self` (the public `Cage` writes still take `&mut self`). This
  is sound because the cage is not `Sync` and nothing ever borrows cage
  memory: every access copies through a raw pointer from the base.
  Reference stores use `write_ref` / `write_value_bits`, which call the
  (empty) write barrier; plain `write_u*` is for data. Roots:
  `cx.scope(|s| …)` takes a closure generic over the scope lifetime
  `'s` (invariant), `s.root(h)` returns a `Copy` `Root<'s, T>` (an index
  plus the context's id) that can't leave the closure; opening a nested
  scope borrows the outer `Scope` mutably, so roots only go on the
  innermost scope, and the scope truncates the stack on exit (also on
  unwind, in `Drop`). Because `Scope` derefs mutably to `Cx`, safe code
  can `mem::swap` another context in; a scope remembers its context's id
  and panics on rooting or closing through a swapped context instead of
  truncating the wrong stack (found by the independent review). `Global<T>` shares the table through `Rc<RefCell<…>>`
  and frees its slot on drop. The handle stack and the global table are
  `RefCell`s, so rooting needs only `&Scope`. `T` in `Gc<'a, T>` is a
  type marker only; layouts are checked by `ferrojs-vm`'s `cells!`
  table. `JsValue<'a>` and rooting of values (not just cell references)
  come with the value-carrying layers (1.7, 1.8). Compile-fail tests are
  `compile_fail` doctests on `gc::RootingRules`, each with a passing
  twin, because rustdoc checks error codes only on nightly.
- **Accepted costs:** a `Gc` doesn't know its context (4 bytes), so a
  handle used with the wrong `Cx` is caught only by the checked
  accessors; `Root` and `Global` check at run time. A stale `Unbound` is
  detected by `bind` (with the verifier or stress mode on) only if its
  slot wasn't
  reused by then; generation tags could close this later if it bites.
  `RefCell` flag checks on rooting; the frame stack, not the handle
  stack, will hold most roots.
- **Rejected:** writes through `&mut Cx` (every write would first have to
  drop all live handles); a separate GC token not borrowing `Cx` (the
  Nova design: contradicts D-023's "handles borrow `Cx` shared" and lets
  `&mut Cx` code run while handles are live); `trybuild` for compile-fail
  tests (a new dependency, and slower); `RefCell<Heap>` (a flag check on
  every field access in the interpreter's hot path).
- **Why:** the borrow checker enforces the rooting rules of D-023 with no
  new `unsafe` beyond three-line `&self` stores, and moving collection
  later only changes what `Root::get` and `Global::get` read.

## D-031 Op definitions as built: per-op handlers, context trait, assembler layout
- **Status:** Accepted (autopilot, 2026-11-21)
- **Choice:** task 1.4 implements D-019/D-020 in the new `ferrojs-bytecode`
  crate. `define_ops!` (a crate-private `macro_rules!` in `define.rs`,
  invoked once in `ops.rs`) takes `Name(snake) { field: Kind, … }
  flags(…) handler(cx) { … }` per op; the snake name is written out
  because `macro_rules!` can't change case. It generates `Opcode`
  (numbered 0.. in definition order; `ALL`, `from_byte`, `info`), the
  metadata table (`OpInfo`: name, operand kinds, `Flags`, unit count),
  `operands::<Name>` structs, the decoded `Instruction` enum with
  `encode` and `for_each_operand`, the decoder, one `Assembler` method
  per op, and one generic handler function per op in `handlers` plus the
  `execute` dispatch. Encoding: prefix bytes `Wide` = 0xFE and
  `ExtraWide` = 0xFF (opcodes must stay below them, checked at compile
  time); an `EnvSlot` is two units; `Imm` and `Label` units are signed.
  A `Label` is relative to the jumping instruction's first byte,
  including its prefix. The assembler takes `AsmLabel`s and lays out at
  `finish`: every instruction starts at its narrowest width and only
  widens while a jump offset doesn't fit, so layout reaches a fixed point.
  Flags: `may_throw`, `may_gc`, `has_ic`, `jump`, `branch`, `loop_edge`,
  `terminator`. The verifier additionally requires that only `loop_edge`
  ops (today `JumpLoop`) jump backward or to themselves and that they
  never jump forward, so every loop passes an interrupt check (D-007);
  plain jumps must go strictly forward. `EnvSlot`s are checked at run
  time, not by the verifier. Handlers see the machine through the
  `OpCtx` trait: `reg`/`set`/`constant`/`note`/`throw`/
  `check_interrupt`/`env_get`/`env_set` plus named slow paths `rt_*`
  returning `Completion = Result<Value, Thrown>` (the exception is held
  by the context), and return `Flow` (`Next`, `Jump`, `Return`,
  `Throw`). `ferrojs_bytecode::testing` has a numbers-only reference
  context and dispatch loop used by tests, the `bytecode` benchmarks and
  the `bytecode_verify` fuzz target (verified random code must run on it
  without panicking). `ferrojs-value` gained `as_int32` and `as_bool`.
- **Rejected:** slow paths behind a closure (`cx.slow(dst, |rt| …)` in
  the sketch in ARCHITECTURE.md): a trait method per runtime function is
  simpler to instantiate per tier and to call from stencils; a single
  `match` with inline handler bodies (the stencil compiler needs each
  handler as its own function); fixing jump widths at emit time (forward
  jumps would all need the widest form); exporting the macro (only this
  crate defines ops).
- **Why:** everything per-op comes from one list, and the verifier's
  rules are exactly what the interpreter relies on, which the fuzz target
  checks.

---

The full set of planned decisions (GC, frames, tail calls, baseline tier,
warm starts, concurrency, platforms, parser, accuracy scope) is in the
planning document and gets logged here as each is implemented.

## D-032 Front end as built: oxc 0.158, `ferrojs-compiler`, parse-only Test262 runs
- **Status:** Accepted (autopilot, 2026-11-21)
- **Choice:** task 1.5 implements D-018 in the new `ferrojs-compiler`
  crate. `parse(&Allocator, source, Goal::{Script, Module})` runs
  `oxc_parser` (regular-expression parsing on, so pattern early errors
  are reported), then `oxc_semantic` with syntax-error checks, then our
  `early::check` pass, and returns the AST plus `Semantic` or every
  `SyntaxError` (message and byte offset). `check` is the AST-free
  convenience. All oxc crates are pinned with `=0.158.0`, the newest
  release whose `rust-version` (1.97) the sandbox toolchain meets; the
  `Allocator` is re-exported so callers need no oxc dependency. The
  crates that depend on oxc (`ferrojs-compiler`, `ferrojs-test262`,
  `ferrojs-diff`, `ferrojs-bench`) declare `rust-version = "1.97"`
  themselves; the workspace value stays 1.80 for now, because raising
  it turns on MSRV-gated clippy lints in `ferrojs-core`, which would make
  this a core change (Miri, review). Raising it is a later small task.
  The Test262 `ferrojs` engine now parses every run: an early error is
  reported as a parse-phase `SyntaxError`; a negative parse-phase test
  that parses is a **failure** (no engine behavior could still pass it);
  everything else that parses is "not run" until the interpreter lands.
  Module jobs check the prelude as a script and the body as a module.
  Result at the pinned suite: 8,699 runs pass (every negative
  parse-phase run), 7 fail.
- **Known gap:** oxc rejects call expressions as assignment targets in
  sloppy code (`f() = 1`, `for (f() in x)`, `f()++`), which the current
  spec allows as a runtime `ReferenceError` (the 7 Annex B
  `assignmenttargettype/callexpression*` failures). Fixing it means
  accepting that oxc diagnostic for a call-expression target in sloppy
  code and emitting a runtime throw; that belongs with the bytecode
  compiler (1.6), which has to handle the throw anyway.
- **Early-error pass:** empty at this pin, since oxc misses no early
  error Test262 checks. Rules are added only with a test that fails
  without them.
- **Also fixed:** the Test262 runner wrote `--write-baseline` before
  reading `--baseline`; the gate passes the same file to both, so it
  compared every run with itself and could never report a regression. It
  now reads first, never rewrites the baseline on a run with regressions,
  and keeps entries a filtered run didn't cover.
- **Rejected:** raising the workspace `rust-version` in this task (core
  lint churn, see above); reporting parse-only successes of negative
  tests as "not run" (hides parser gaps from the dashboard); our own AST
  (D-018).

## D-033 Bytecode compiler, part 1a: registers, static TDZ, shared number semantics
- **Status:** Accepted (autopilot, 2026-11-21)
- **Choice:** task 1.6 is split into 1.6a–c (roadmap). 1.6a adds
  `ferrojs_compiler::codegen`: `compile_function(&Parsed, &Function)`
  produces a `CompiledFunction` (name, `params` P, `registers` R, number
  constant pool, IC count, strict flag, code). Registers: parameters are
  `r0..rP` (a repeated sloppy-mode name binds the last one); every other
  binding owned by the function (its scope, or block scopes under it,
  but not nested functions) gets a fixed register in symbol order;
  temporaries are a stack above the locals. A plain local read uses the
  local's register directly; a left operand is copied to a temporary
  when the right side may assign. `expr_to(e, dst)` may write `dst`
  early, so assignments go straight into a local only for expressions
  that write their destination last and don't mention it. Int32
  literals use `LoadInt`; other numbers are pooled by bit pattern.
  Unresolved `undefined`, `NaN` and `Infinity` compile to constants
  (safe while `with` and direct `eval` are unsupported).
- **TDZ in 1.6a is static:** a `let`/`const` read before its
  declaration has been compiled, or any read of a function, class or
  catch binding, is rejected as unsupported rather than checked at run
  time. Within one function without closures or `switch`, textual order
  is execution order for lexical bindings, so this never accepts a TDZ
  violation. 1.6b replaces it with the marker value and run-time checks.
  Assignment to `const` is likewise unsupported until 1.6b emits the
  `TypeError`.
- **Ops added** (at the end of the list; no cache format exists yet):
  `Div Rem Exp BitAnd BitOr BitXor Shl Shr UShr` (one slow path,
  `rt_numeric(NumericOp, …)`), `GreaterThan LessEqual GreaterEqual`
  (`rt_compare(CompareOp, …)`), `ToNumber ToNumeric Negate BitNot Inc
  Dec` (`rt_unary(UnaryOp, …)`), `Equal` (`rt_loose_equal`) and
  `JumpIfNotNullish`. `!=` and `!==` are the equality op plus `Not`.
  Grouping the slow paths by an operator enum keeps `OpCtx` small; the
  fast paths stay per op. Number semantics (ToInt32, ToUint32,
  exponentiate, remainder, shifts, `-0` rules) live once in
  `ferrojs_bytecode::number` and both the fast paths and every slow path
  call them, so tiers can't disagree.
- **Toy context:** the slow paths now do ToNumber on `undefined`, `null`
  and booleans (heap values still throw), so compiled code can be
  checked against Node.js: the compiler's tests run 90-odd calls on the
  toy and compare bit-exact results with `node`.
- **Benchmark slowdown accepted:** growing the op list from 19 to 41
  ops makes the generic `decode` +10% and the toy `dispatch` loop +6% in
  instruction counts: LLVM inlines the larger generated `match`es
  differently. Tried `#[inline]` and a decoder specialized per operand
  width; both made `decode` worse. These benchmarks exercise the 1.4
  scaffolding (the toy loop and the generic decoder), not the real
  interpreter, which task 1.7 designs and benchmarks on its own; the
  baseline was rewritten in this commit.
- **Rejected:** an accumulator (D-019); allocating locals lazily
  (breaks the temporary stack); compiling a whole script before globals
  exist (top-level bindings must be global, ARCHITECTURE.md §8);
  separate `rt_*` functions per new op (a dozen near-identical trait
  methods).

## D-034 TDZ marker and run-time TDZ checks only where needed
- **Status:** Accepted (autopilot, 2026-11-21)
- **Choice:** the internal "uninitialized" marker is `TAG_OTHER` with the
  reserved kind `0xFFFF` and offset 0 (`Value::UNINITIALIZED`,
  `Kind::Uninitialized`, `is_uninitialized`). `from_bits_checked`
  rejects it, so cache input can't contain it; it is not a heap
  reference, so a GC that scans value slots by kind skips it. New ops:
  `LoadUninitialized dst`, `CheckInitialized src` (throws a
  `ReferenceError` through `rt_throw_error(ErrorKind::UninitializedBinding)`)
  and `ThrowConstAssign` (a terminator; `TypeError`). The compiler
  checks only bindings that need it: a `let`/`const` gets the marker at
  scope entry and a check at every use only if some use starts before
  its declarator ends (found by an `oxc_ast_visit` pass over the body),
  or if it is declared in a `switch` case block, where a case can jump
  past the declaration. Every other use is proven to run after
  initialization, because within one function without closures textual
  order is execution order for lexical bindings. Closures (1.6d) add
  their own rule: uses from a nested function always check unless the
  binding is initialized before the closure is created.
- **Order of checks** follows the spec: an assignment evaluates its
  right side first, then SetMutableBinding does the TDZ check
  (`ReferenceError`) and then the `const` check (`TypeError`); compound,
  logical and update assignments read (and check) the target first, and
  a short-circuiting logical assignment to a `const` doesn't throw.
- **Rejected:** checking every lexical use (a check per read in hot loops
  for no benefit); building oxc's full `AstNodes` table to get reference
  positions (memory and time for every parse, when one visitor over the
  function body suffices).

## D-035 Exceptions: forward-only handler table, inline finally
- **Status:** Accepted (autopilot, 2026-11-21)
- **Choice:** a compiled function carries `handlers: Vec<Handler>`
  (`ferrojs_bytecode::Handler { start, end, target, exception: Reg }`,
  byte offsets), innermost ranges first: entries are emitted when their
  range closes, and inner ranges close first. `verify_handlers` requires
  non-empty ranges on instruction boundaries, targets on instructions at
  or after the range end (so a throw always moves forward and every loop
  still passes `JumpLoop`'s interrupt check), and registers in range.
  `Assembler::finish_with_labels` gives label offsets to build the table.
  `try/catch`: the catch parameter's register receives the exception
  (a temporary for `catch {}`). `finally` is compiled once, inline, with
  two temporaries: a completion kind (0 normal, 1 throw, 2 return, 3+i
  the i-th `break`/`continue` routed through it) and a value; the try and
  catch blocks are covered by a handler that stores the exception in the
  value register and sets kind 1. After the finally block, compares on
  the kind rethrow, return (through the enclosing finally if any) or
  re-issue the jump (again through enclosing finallys), so every exit
  path runs each finally exactly once. Breakables record how many
  finallys were open when entered to know when a jump crosses one.
- **Termination isn't catchable:** a throw caused by the interrupt
  check (D-007) skips handlers. The toy context records it in
  `terminating`; the interpreter (1.7) must do the same.
- **Not yet:** `var` redeclaring a catch parameter (Annex B.3.4) is
  rejected as unsupported until it is handled deliberately; destructuring
  catch parameters wait for destructuring.
- **Rejected:** duplicating the finally block at every exit (code size
  grows with exits and nesting); subroutine-style `jsr`/`ret` (needs an
  indirect jump the verifier can't check); backward handler targets.
