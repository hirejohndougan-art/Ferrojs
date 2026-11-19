# FerroJS roadmap and task queue

The autopilot works through this file top to bottom (see `docs/AUTOPILOT.md`).
Phases come from `docs/PLAN.md`. Only the current phase is broken into
run-sized tasks; the monthly review breaks down the next one.

Status marks: `[ ]` to do · `[~]` in progress · `[x]` done · `[!]` stuck.
`(risky)` means an independent review is required before merging.
`needs: X.Y` lists tasks that must be done first.

## Timeline targets (from the plan; rough)

| Phase | Target | Gate to pass before the next phase |
| --- | --- | --- |
| 0. Quality system | Dec 2026 | Differential harness, fuzzing and benchmarks run in the gate |
| 1. Core engine | Jan–Feb 2027 | Runs real scripts; interpreter within 2× QuickJS |
| 2. Full language and built-ins | Mar–Aug 2027 | Over 95% of Test262 |
| 3. Baseline JIT and inline caches | Sep–Oct 2027 | Faster than QuickJS and Boa on the suite |
| 4. Warm starts, embedding API 1.0 | Nov 2027 | Repeat-run startup ahead of V8 with code cache |
| 5. Optimizing tier, moving GC | 2028 | Steady climb toward V8 parity |
| 6. WebAssembly | Alongside 5 | Wasm spec tests and JS API tests |
| 1.0 release | ~9–12 months in | 100% Test262 for the current edition, adoption items 1–5, 7, 8 |

## Done before the autopilot started

- [x] Value representation, NaN-boxing (`ferrojs-value`, D-001)
- [x] CI: fmt, clippy, tests on Linux, macOS, Windows
- [x] Test262 runner, pinned suite, baseline gate (D-011)

## Phase 0 — Finish the quality system

- [x] 0.1 (2026-11-19) Differential harness: run a JS file on FerroJS and Node.js (via the
  existing Node driver) and compare printed output and thrown errors. Add a
  `tests/diff/` corpus and a gate step. Until FerroJS runs JS, the harness
  is tested with Node vs Node.
- [x] 0.2 (2026-11-19) Fuzzing scaffold: `cargo-fuzz` targets for `ferrojs-value`
  (`from_bits_checked`, round trips). Short smoke run in the gate; document
  how to run long sessions. Later targets get added as components land.
- [x] 0.3 (2026-11-19) Benchmark harness: `iai-callgrind` (now `gungraun`, D-015) instruction-count benchmarks
  in a `ferrojs-bench` crate, with a stored baseline and a 3% regression
  check in the gate. Start with value encoding micro-benchmarks.
- [ ] 0.4 Miri job on `ferrojs-core` (empty for now, but wired into the gate
  and CI so every later `unsafe` addition is checked).
- [ ] 0.5 Progress dashboard: a script that writes Test262 pass counts per
  directory and benchmark numbers to `docs/reports/dashboard.md`.

## Phase 1 — Core engine

Order matters: each task builds on the ones above it.

- [ ] 1.1 Architecture spec: write `docs/ARCHITECTURE.md` covering frames,
  bytecode format, op-definition language, heap layout, GC interface,
  shapes and inline caches, in enough detail for later runs. Log the
  remaining planned decisions (GC, frames, tail calls, parser) in
  `DECISIONS.md`.
- [ ] 1.2 (risky) Heap cage in `ferrojs-core`: reserve the 4 GB region,
  32-bit offsets, bump allocation, guard pages (D-002). needs: 1.1
- [ ] 1.3 (risky) Simple non-moving mark-sweep GC behind lifetime-branded
  handles (`gc-arena` style); no code may assume objects stay put. GC
  stress mode (collect on every allocation) from the start. needs: 1.2
- [ ] 1.4 Op-definition language: a Rust macro that defines each bytecode
  op once and generates the interpreter handler (later also the
  copy-and-patch stencil and optimizer lowering). needs: 1.1
- [ ] 1.5 Parser integration: oxc AST in, plus an early-error pass. Run
  Test262's syntax (negative parse) tests through it. needs: 1.1
- [ ] 1.6 Bytecode compiler, part 1: expressions, variables, functions,
  control flow; register-based bytecode. needs: 1.4, 1.5
- [ ] 1.7 Interpreter loop with interrupt checks (D-007) and the shared frame
  layout; proper tail calls designed in. needs: 1.3, 1.6
- [ ] 1.8 Objects and shapes (hidden classes), property get/set, prototype
  chains. needs: 1.3
- [ ] 1.9 Minimal embedding API and a `ferrojs` command-line binary that
  runs a script and prints uncaught errors. needs: 1.7
- [ ] 1.10 Hook the Test262 runner's `ferrojs` engine to the real engine;
  `print`, `$262` host object. First real baseline. needs: 1.9
- [ ] 1.11 Host hooks for randomness, clocks and job ordering (D-008).
- [ ] 1.12 Bytecode compiler, part 2: classes, destructuring, spread,
  closures, generators and async (state machines). needs: 1.6
- [ ] 1.13 Basic built-ins: Object, Function, Error types, Array, String,
  Number, Boolean, Math, JSON (hand-written; spec generation comes in 2).
- [ ] 1.14 Promise and the job queue.
- [ ] 1.15 Benchmarks against QuickJS (or the closest available engine);
  record the interpreter-vs-QuickJS ratio. Phase 1 gate: real scripts run
  and the interpreter is within 2× of QuickJS.

## Phase 2 — Full language and built-ins (broken down at the monthly review)

Spec-generated built-ins from ECMA-262 pseudocode (fallback: hand-written),
RegExp via `regress`, Proxy and Reflect, typed arrays and ArrayBuffer,
Map/Set/WeakMap/WeakRef, iterators and generators complete, modules,
Annex B, built-in TypeScript type stripping, debugger hooks, WebAssembly
build target. Gate: over 95% of Test262.

## Phase 3 onward

See `docs/PLAN.md` (phased roadmap, adoption features). Broken down by the
monthly review when Phase 2 nears its gate.
