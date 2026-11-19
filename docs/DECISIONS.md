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

---

The full set of planned decisions (GC, frames, tail calls, baseline tier,
warm starts, concurrency, platforms, parser, accuracy scope) is in the
planning document and gets logged here as each is implemented.
