# FerroJS progress log

One entry per autopilot run, newest at the bottom. Format is in
`docs/AUTOPILOT.md`.

## 2026-11-19 — Autopilot set up
- Result: setup
- Changed: added `docs/AUTOPILOT.md`, `docs/ROADMAP.md`, `docs/PLAN.md`,
  this log and `CLAUDE.md`; decision D-012 (fully autonomous development).
- Decisions: D-012
- Test262: 0 (engine can't run JS yet)
- Next: 0.1 differential harness

## 2026-11-19 09:30 UTC — 0.1 Differential harness
- Result: merged
- Changed: new `ferrojs-diff` crate (`jsdiff` tool) comparing printed output
  and uncaught errors against Node.js; 17-file corpus in `tests/diff/`; two
  gate steps (node vs node, ferrojs vs node) in AUTOPILOT.md and CI.
- Decisions: D-013
- Test262: 0 (+0; engine can't run JS yet)
- Next: 0.2 fuzzing scaffold

## 2026-11-19 11:18 UTC — 0.2 Fuzzing scaffold
- Result: merged
- Changed: `fuzz/` libFuzzer crate with `value_bits` and `value_roundtrip`
  property targets; `tools/fuzz.sh` builds them on stable Rust (no nightly)
  and runs a 10 s-per-target smoke as a new gate step and CI job.
- Decisions: D-014
- Test262: 0 (+0; engine can't run JS yet)
- Next: 0.3 benchmark harness

## 2026-11-19 15:27 UTC — 0.3 Benchmark harness
- Result: merged
- Changed: `ferrojs-bench` crate with five gungraun (ex-iai-callgrind)
  value-encoding benchmarks, `benchcheck` tool, stored baseline in
  `conformance/bench-baseline.txt`; `tools/bench.sh check --write` is a new
  gate step and CI job (fails on >3% more instructions). Follow-up commit
  makes the gungraun dependency and benchmarks Linux-only so macOS and
  Windows CI still build all targets.
- Decisions: D-015
- Test262: 0 (+0; engine can't run JS yet)
- Next: 0.4 Miri job on ferrojs-core

## 2026-11-19 16:05 UTC — 0.4 Miri job on ferrojs-core
- Result: merged
- Changed: `tools/miri.sh` (strict provenance, canary test that only exists
  under Miri) as a gate step, plus a CI `miri` job on nightly. Nightly can't
  be downloaded in the autopilot sandbox, so core changes wait for the CI
  job; Miri passed in CI run 44561192248 for this change.
- Decisions: D-016
- Test262: 0 (+0; engine can't run JS yet)
- Next: 0.5 progress dashboard

## 2026-11-20 07:18 UTC — 0.5 Progress dashboard
- Result: merged
- Changed: `tools/dashboard.sh` and a tested `dashboard` binary in
  `ferrojs-test262` write `docs/reports/dashboard.md` (Test262 per
  top-level and second-level directory, benchmark counts); first page
  committed. `default-run` keeps the gate's `cargo run -p ferrojs-test262`
  working. Phase 0 is complete.
- Decisions: D-017
- Test262: 0 (+0; engine can't run JS yet)
- Next: 1.1 architecture spec. Weekly report runs should regenerate the
  dashboard with `tools/dashboard.sh` (D-017).

## 2026-11-20 09:23 UTC — 1.1 Architecture spec
- Result: merged
- Changed: new `docs/ARCHITECTURE.md` (crates, frames and tail calls,
  bytecode and op-definition macro, heap cage and cells, GC handles and
  roots, shapes and CacheIR inline caches, environments, host hooks).
  Roadmap 1.3 and 1.5 point at the new decisions. Docs only.
- Decisions: D-018 parser (oxc, pinned by MSRV), D-019 bytecode, D-020 op
  definitions, D-021 frames and tail calls, D-022 heap layout, D-023 GC
  interface, D-024 shapes and ICs
- Test262: 0 (+0; engine can't run JS yet)
- Next: 1.2 heap cage (risky; core change, needs CI Miri). Note oxc 0.159
  needs rustc 1.98; the sandbox has 1.97 (see D-018 for 1.5).

## 2026-11-20 11:40 UTC — 1.2 Heap cage
- Result: merged
- Changed: `ferrojs-core` cage (4 GB + 64 KB guard reserved via mmap /
  VirtualAlloc, 256 KB chunks committed on demand, chunk 0 never
  committed, checked 32-bit-offset accessors) and a bump-allocating `Heap`
  with chunk runs for cells over 8 KB; new `heap/bump_alloc` benchmark.
  Independent review found no bugs; its should-fixes (no `&mut Cage` from
  `Heap`, `is_full_size`, OOM reports requested size) are in.
  Miri: passed in CI run 44727937387 (Windows and macOS jobs too).
- Decisions: D-027
- Test262: 0 (+0; engine can't run JS yet)
- Next: 1.3 mark-sweep GC (risky; core change, needs CI Miri). 1.4 and
  1.5 are also unblocked.

## 2026-11-21 00:00 UTC — 1.3a Collector core (1.3 split into 1.3a/1.3b)
- Result: merged
- Changed: `ferrojs-core::heap` is now a mark-sweep collector: size-class
  chunks with Rust-side free lists, large cells in chunk runs, a
  free-chunk pool (runs may extend into fresh chunks), side mark bitmaps,
  explicit mark stack, poisoning, a `Trace` callback, and a heap verifier
  that also checks the chunk table, pool, free lists and byte count.
  Writes are kept out of chunk headers; a collection that panicked is
  cleaned up at the next one. Two randomized model-checked stress tests;
  new `collect` benchmark. Independent review: one bug (stale mark bits
  after a panic) and several should-fixes, all fixed or recorded in D-028.
  Miri: passed in CI run 44839178765. That run's bench job fails by
  design (it compares with main; the slowdown is explained in D-028).
  Started at John's request outside the nightly schedule.
- Decisions: D-028
- Test262: 0 (+0; engine can't run JS yet)
- Next: 1.3b branded handles (risky; core change, needs CI Miri). 1.4
  and 1.5 are also unblocked.

## 2026-11-21 07:40 UTC — 1.3b Branded handles
- Result: merged
- Changed: new `ferrojs-core::gc`: `Cx` (heap, tracer, handle stack,
  global table), `NoGc` token, `Gc<'a, T>`, `Unbound<T>`, scoped
  `Root<'s, T>` via `cx.scope`, `Global<T>`, `write_ref` /
  `write_value_bits` with an empty write barrier, GC stress mode
  (`FERROJS_GC_STRESS=1`). Heap writes now take `&self` through
  crate-private cage stores. Compile-fail doctests for the rooting rules
  (each with a passing twin), a model-checked random-graph stress test,
  new `cx_alloc` benchmark. Independent review: no memory-safety bugs;
  one rooting hole (swapping a `Cx` out of a scope truncated the wrong
  stack) and a stress-mode gap, both fixed with tests.
  Miri: passed in CI run 44880468300.
- Decisions: D-030
- Test262: 0 (+0; engine can't run JS yet)
- Next: 1.4 op-definition language (1.5 parser is also unblocked; 1.8
  objects and shapes now too). Tomorrow is Sunday: the weekly report is due.

## 2026-11-21 10:05 UTC — 1.4 Op-definition language
- Result: merged
- Changed: new `ferrojs-bytecode` crate: `define_ops!` generates from
  one op list the opcode enum and metadata, operand structs, decoded
  `Instruction`, encoder/decoder, an `Assembler` method per op (jump
  widths fixed at `finish`), and a generic handler per op over `OpCtx`
  plus `execute`. Verifier (ranges, jump targets on boundaries, only
  `JumpLoop` jumps backward, no fall-through at the end) and
  disassembler. 19 starter ops (loads, arithmetic, compare, jumps, env).
  Numbers-only `testing::ToyCtx` drives tests, new `bytecode` benchmarks
  (decode, dispatch loop) and the `bytecode_verify` fuzz target.
  `ferrojs-value` gains `as_int32` / `as_bool`. Not risky and
  `ferrojs-core` unchanged, so no independent review or CI Miri needed.
- Decisions: D-031
- Test262: 0 (+0; engine can't run JS yet)
- Next: 1.5 parser integration (oxc; check D-018 pinning against the
  toolchain), or 1.8 objects and shapes. Tomorrow is Sunday: the weekly
  report is due first.
