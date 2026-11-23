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

## How overall completion is measured

The daily update reports one percentage for the whole project (to 1.0 plus
V8-competitive speed). Each phase has a fixed weight, roughly its share of
the total work; a phase counts as done in proportion to its progress.

| Phase | Weight | Progress within the phase |
| --- | --- | --- |
| Planning and setup (done before autopilot) | 2% | done |
| 0. Quality system | 3% | finished tasks ÷ tasks |
| 1. Core engine | 12% | finished tasks ÷ tasks |
| 2. Full language and built-ins | 35% | Test262 pass rate ÷ 95%, capped at 100% (until then, finished tasks ÷ tasks if that is higher) |
| 3. Baseline JIT and inline caches | 10% | finished tasks ÷ tasks |
| 4. Warm starts, embedding API 1.0 | 5% | finished tasks ÷ tasks |
| 5. Optimizing tier, moving GC | 23% | finished tasks ÷ tasks |
| 6. WebAssembly | 7% | finished tasks ÷ tasks |
| Close to 100% Test262 and 1.0 release | 3% | (Test262 pass rate − 95%) ÷ 5% |

A phase with no task list yet counts as 0%. When the monthly review breaks a
phase into tasks, the weights stay the same. Change weights only in a
monthly review, with the reason logged, so the number stays comparable from
day to day.

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
- [x] 0.4 (2026-11-19) Miri job on `ferrojs-core` (empty for now, but wired into the gate
  and CI so every later `unsafe` addition is checked).
- [x] 0.5 (2026-11-20) Progress dashboard: a script that writes Test262 pass counts per
  directory and benchmark numbers to `docs/reports/dashboard.md`.

## Phase 1 — Core engine

Order matters: each task builds on the ones above it.

- [x] 1.1 (2026-11-20) Architecture spec: write `docs/ARCHITECTURE.md` covering frames,
  bytecode format, op-definition language, heap layout, GC interface,
  shapes and inline caches, in enough detail for later runs. Log the
  remaining planned decisions (GC, frames, tail calls, parser) in
  `DECISIONS.md`.
- [x] 1.2 (2026-11-20) (risky) Heap cage in `ferrojs-core`: reserve the 4 GB region,
  32-bit offsets, bump allocation, guard pages (D-002). needs: 1.1
- [x] 1.3a (2026-11-20) (risky) Collector core in
  `ferrojs-core`: size-class chunks with free lists (D-022), side mark
  bitmaps, explicit mark stack, sweep with poisoning, chunk reuse, a
  `Trace` callback for cell layouts, heap verifier, and a randomized
  collect-on-every-allocation stress test. Roots are passed in by the
  caller. needs: 1.2
- [x] 1.3b (2026-11-21) (risky) Lifetime-branded handles on top of 1.3a (D-023): `Cx`,
  `NoGc`, `Gc<'a, T>`, `Unbound<T>`, scoped `Root`s on a handle stack,
  `Global` table, the write-barrier API, and GC stress mode (collect on
  every allocation) wired into `Cx`; no code may assume objects stay put.
  Compile-fail tests for the rooting rules. needs: 1.3a
- [x] 1.4 (2026-11-21) Op-definition language: a Rust macro that defines each bytecode
  op once and generates the interpreter handler (later also the
  copy-and-patch stencil and optimizer lowering). needs: 1.1
- [x] 1.5 (2026-11-21) Parser integration: oxc AST in, plus an early-error pass. Run
  Test262's syntax (negative parse) tests through it. Pin oxc per D-018
  (newest release whose `rust-version` the toolchain meets). needs: 1.1
- [x] 1.6a (2026-11-21) Bytecode compiler, part 1a: function bodies with
  parameters and uncaptured locals in registers; number, boolean, null
  and `undefined` literals; unary, binary, logical, conditional,
  assignment, compound assignment and update expressions; `if`, loops,
  labels, `break`/`continue`, `return`, `throw`. Adds the arithmetic,
  comparison, equality and bitwise ops it needs. Anything else is a
  clean "unsupported" error. needs: 1.4, 1.5
- [x] 1.6b (2026-11-21) Bytecode compiler, part 1b: the TDZ marker value
  and run-time TDZ checks where the compiler can't prove them away,
  assignment to `const` (TypeError), `switch`. needs: 1.6a
- [x] 1.6c (2026-11-21) Bytecode compiler, part 1c: `try`/`catch`/`finally` with the
  exception table (ARCHITECTURE.md §3.6), including `break`, `continue`
  and `return` through `finally`. needs: 1.6b
- [x] 1.6d (2026-11-21) Bytecode compiler, part 1d: nested functions,
  closures and calls (`CreateClosure`, `Call`/`TailCall` returning to
  the dispatch loop per ARCHITECTURE.md §3.3–3.4), captured variables in
  environments, per-iteration `let` bindings; the toy context grows
  frames, closures and environments to run them. needs: 1.6c
- [x] 1.6e (2026-11-23) Bytecode compiler, part 1e: scripts
  and globals: `compile_script`, global var/function/let/const
  declarations, `LoadName`/`StoreName` for top-level and undeclared
  names (ReferenceError, sloppy implicit globals), global TDZ and const,
  `this` in strict functions; the toy context grows globals and interned
  name strings. needs: 1.6d
- [ ] 1.6f Bytecode compiler, part 1f: string values: string literals and
  templates in the constant pool, `typeof`, `+` concatenation and
  Number::toString in the toy. needs: 1.6e
- [ ] 1.6g Accept sloppy-mode call expressions as assignment targets
  (runtime `ReferenceError`; the 7 Annex B failures, D-032). needs: 1.6e
- [ ] 1.7a (risky; done on branch `auto/2026-11-23-core-handles`, reviewed,
  waiting for a CI Miri run: GitHub Actions has started no job since
  2026-11-21, likely out of minutes) Core handle API for the interpreter: a checked way to
  turn a raw cell offset held in a register `Value` into a `Gc` handle
  (`NoGc::handle(offset) -> Option<Gc>`, live-cell check under the
  verifier), with tests. Every VM task needs it: registers hold raw
  values (ARCHITECTURE.md §2) and cells are only readable and writable
  through `Gc` handles. Core change: CI Miri required. needs: 1.3b
- [ ] 1.7 Interpreter loop with interrupt checks (D-007) and the shared frame
  layout; proper tail calls designed in. Dispatch cost must not grow
  with the number of ops (D-036): benchmark it. Plan (from the 1.6d
  run): new `ferrojs-vm` crate; the frame stack is shared `Cell<u64>`
  slots outside the cage, made a GC root by a small anchor cell held in
  a `Global` whose trace callback pushes every reference on the live
  part of the stack (raw header slots stay below 2^48, so they never
  look like references); closures and environments are heap cells;
  templates stay in a Rust-side table indexed from the closure header
  (no raw pointers in cells, D-009); reuse the compiler's Node.js cases
  and run them under GC stress. needs: 1.3b, 1.6d, 1.7a
- [x] 1.8a (2026-11-23) Objects and shapes, part a: new
  `ferrojs-vm` crate with the cell tracer, string cells and atoms,
  shape transition trees with the prototype, ordinary objects with
  inline and out-of-line slots, data-property get/set/define, model-
  checked under GC stress. needs: 1.3b
- [x] 1.8b (2026-11-23) Objects, part b: delete,
  attribute changes (ValidateAndApplyPropertyDescriptor for data
  properties), dictionary mode (ARCHITECTURE.md §7.2), non-extensible
  objects. needs: 1.8a
- [ ] 1.8d Objects, part d: accessor properties (getters and setters
  called from get/set), the lazy key table for large shapes. needs: 1.8b, 1.7
- [x] 1.8c (2026-11-23) Objects, part c: indexed properties in the elements store
  (packed, holey, sparse) and Array length. needs: 1.8a
- [ ] 1.9 Minimal embedding API and a `ferrojs` command-line binary that
  runs a script and prints uncaught errors. needs: 1.7
- [ ] 1.10 Hook the Test262 runner's `ferrojs` engine to the real engine;
  `print`, `$262` host object. First real baseline. needs: 1.9
- [x] 1.11 (2026-11-23) Host hooks for randomness, clocks and job ordering (D-008).
- [ ] 1.12 Bytecode compiler, part 2: classes, destructuring, spread,
  closures, generators and async (state machines). needs: 1.6d
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

Always included when Phase 2 is broken down (requested by John, 2026-11-20):

- **Real-world code corpus.** A `realworld/` suite of popular npm packages
  (start with React and ReactDOM server rendering, jQuery on a minimal DOM
  stub, lodash, Vue, the TypeScript compiler, Babel, a bundler such as
  esbuild-wasm or Rollup, marked, moment/date-fns, a JSON-heavy workload)
  plus JetStream, Octane and SunSpider from GitHub. Each entry runs on
  FerroJS and Node.js through the differential harness (same output, same
  errors) and, where the package has its own test suite, runs that suite
  too. Pass count per package goes on the dashboard. Packages come from the
  npm registry, which the run environment can reach; pin exact versions.
- **Real-world gate.** Once a package passes, it joins the gate: it must
  keep passing, like the Test262 baseline.
- **Real-world tuning.** Profile the corpus and the benchmarks against V8,
  QuickJS and Boa weekly; slow spots become roadmap tasks.
- Live websites need a browser (Ironswift) and open internet; that work is
  planned after FerroJS can be embedded in Ironswift.

## Phase 3 onward

See `docs/PLAN.md` (phased roadmap, adoption features). Broken down by the
monthly review when Phase 2 nears its gate.
