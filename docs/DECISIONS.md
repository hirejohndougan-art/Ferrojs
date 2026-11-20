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

---

The full set of planned decisions (GC, frames, tail calls, baseline tier,
warm starts, concurrency, platforms, parser, accuracy scope) is in the
planning document and gets logged here as each is implemented.
