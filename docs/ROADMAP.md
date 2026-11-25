# FerroJS roadmap and task queue

The autopilot works through this file top to bottom (see `docs/AUTOPILOT.md`).
Phases come from `docs/PLAN.md`. Only the current phase is broken into
run-sized tasks; the monthly review breaks down the next one.

Status marks: `[ ]` to do · `[~]` in progress · `[x]` done · `[!]` stuck.
`(risky)` means an independent review is required before merging.
`needs: X.Y` lists tasks that must be done first.
Routine and core work, and which model may do which: `docs/AUTOPILOT.md`
step 10a (D-049).

## Timeline targets (from the plan; rough)

| Phase | Target | Gate to pass before the next phase |
| --- | --- | --- |
| 0. Quality system | Dec 2026 | Differential harness, fuzzing and benchmarks run in the gate |
| 1. Core engine | Jan–Feb 2027 | Runs real scripts; interpreter within 2× QuickJS |
| 2. Full language and built-ins | Mar–Aug 2027 (likely later, D-056) | 100% of Test262 for the current edition (D-056) |
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
| 2. Full language and built-ins | 35% | Test262 pass rate (D-056) (until then, finished tasks ÷ tasks if that is higher) |
| 3. Baseline JIT and inline caches | 10% | finished tasks ÷ tasks |
| 4. Warm starts, embedding API 1.0 | 5% | finished tasks ÷ tasks |
| 5. Optimizing tier, moving GC | 23% | finished tasks ÷ tasks |
| 6. WebAssembly | 7% | finished tasks ÷ tasks |
| 1.0 release | 3% | finished release tasks ÷ tasks (none listed yet; Test262 now finishes in Phase 2, D-056) |

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
- [x] 1.6f Bytecode compiler, part 1f: string values: string literals and
  templates in the constant pool, `typeof`, `+` concatenation and
  Number::toString in the toy. needs: 1.6e
- [x] 1.6g (2026-11-23) Accept sloppy-mode call expressions as assignment targets
  (runtime `ReferenceError`; the 7 Annex B failures, D-032). needs: 1.6e
- [x] 1.7a (2026-11-25, risky; reviewed; CI Miri passed in run 45558416159
  once the repo went public) Core handle API for the interpreter: a checked way to
  turn a raw cell offset held in a register `Value` into a `Gc` handle
  (`NoGc::handle(offset) -> Option<Gc>`, live-cell check under the
  verifier), with tests. Every VM task needs it: registers hold raw
  values (ARCHITECTURE.md §2) and cells are only readable and writable
  through `Gc` handles. Core change: CI Miri required. needs: 1.3b
- 1.7 Interpreter (split 2026-11-23, D-047; merged 2026-11-25 with 1.7a):
  - [x] 1.7b (2026-11-23; merged 2026-11-25) Interpreter core: frame stack (ARCHITECTURE.md §3)
    rooted through an anchor cell, function and environment cells, calls,
    proper tail calls, returns, exception unwinding, interrupts, stack
    overflow; every compiled function case matches the toy context, also
    under GC stress. needs: 1.3b, 1.6d, 1.7a
  - [x] 1.7c (2026-11-24; merged 2026-11-25, D-050) Strings and globals in the interpreter: string values beyond
    atoms (concatenation, Number::toString, comparisons), the global
    object and global lexical environment; the script cases (1.6e, 1.6f)
    match Node.js. needs: 1.7b, 1.8b
  - [x] 1.7d (2026-11-25, D-052) Dispatch cost against the op count (D-036): a benchmark that
    shows it stays flat as ops are added. needs: 1.7b
  - [x] 1.7e (2026-11-25, D-053) Property access (added 2026-11-25: nothing on the roadmap
    compiled `o.x` yet, and 1.8d, 1.9, 1.12b–e and 1.13 all need it):
    property ops in the bytecode, object literals with data properties
    and computed keys, `o.x` / `o[k]` reads, writes, compound and update
    assignment, `delete`, `in`, method calls with `this`; in the
    compiler, the toy context and the interpreter (on 1.8's objects).
    Getters, setters, spread and `__proto__` in literals stay
    unsupported until 1.8d and 1.13. needs: 1.7c, 1.8c
- [x] 1.8a (2026-11-23) Objects and shapes, part a: new
  `ferrojs-vm` crate with the cell tracer, string cells and atoms,
  shape transition trees with the prototype, ordinary objects with
  inline and out-of-line slots, data-property get/set/define, model-
  checked under GC stress. needs: 1.3b
- [x] 1.8b (2026-11-23) Objects, part b: delete,
  attribute changes (ValidateAndApplyPropertyDescriptor for data
  properties), dictionary mode (ARCHITECTURE.md §7.2), non-extensible
  objects. needs: 1.8a
- 1.8d Objects, part d (split 2026-11-25: accessors in the object model,
  calling them, and the key table are three run-sized changes):
  - [x] 1.8d (2026-11-25, D-055) Accessor properties in the object model:
    accessor pair cells, `get`/`set` handing back the getter or setter,
    `define_property` with partial descriptors
    (ValidateAndApplyPropertyDescriptor). needs: 1.8b
  - [x] 1.8e (2026-11-25, D-059) Call getters and setters from the
    interpreter's property ops (a frame for the call, D-036), and compile
    `get` / `set` in object literals. Global-name accessors wait for
    1.13's `Object.defineProperty` (none can be made before). needs: 1.8d, 1.7
  - [x] 1.8f (2026-11-25, D-060) The key table for large shapes (hashed lookup for
    shared and dictionary shapes, D-038/D-039 simplifications).
    needs: 1.8b
- [x] 1.8c (2026-11-23) Objects, part c: indexed properties in the elements store
  (packed, holey, sparse) and Array length. needs: 1.8a
- [x] (2026-11-25, D-054; merged 2026-11-25) 1.9 Minimal embedding API and a `ferrojs` command-line binary that
  runs a script and prints uncaught errors. needs: 1.7
- 1.10 Run Test262 on the real engine (split 2026-11-25: Test262's own
  harness, `assert.js` and `sta.js`, needs functions that carry
  properties, `new`, `instanceof` and error objects, none of which
  existed; the old entry only listed 1.9):
  - [x] 1.10a (2026-11-25, risky, D-057) Functions as
    ordinary objects: closure and host function cells get the object
    header (shape, elements, slots), so `f.x = 1` works; the
    `Object.prototype` and `Function.prototype` intrinsics; `prototype`,
    `name` and `length` on functions. Core: object layout. needs: 1.7,
    1.8d, 1.9
  - [ ] 1.10b (risky) `new` and `instanceof` for ordinary functions:
    \[\[Construct\]\] (OrdinaryCreateFromConstructor, `this` in the new
    frame, returning an object), `new.target`, OrdinaryHasInstance.
    needs: 1.10a
  - [ ] 1.10c Error objects: the engine throws real `Error`,
    `TypeError`, `ReferenceError`, `SyntaxError` and `RangeError`
    objects (constructor, `name`, `message`, prototype chain), and the
    constructors exist as globals; a minimal `String()` for values the
    harness prints. The rest of 1.13 stays there. needs: 1.10b
  - [ ] 1.10d Hook the Test262 runner's `ferrojs` engine to the real
    engine; `print`, `$262` host object. First real baseline.
    needs: 1.10c
- [x] 1.11 (2026-11-23) Host hooks for randomness, clocks and job ordering (D-008).
- 1.12 Bytecode compiler, part 2 (split 2026-11-23: everything but
  default values needs objects, iterators or the interpreter):
  - [x] 1.12a (2026-11-23) Default parameter values: left-to-right
    initialization, parameter TDZ, `length` (D-046). needs: 1.6d
  - [ ] 1.12b Rest parameters, spread arguments and array literals.
    needs: 1.7, 1.8c
  - [ ] 1.12c Destructuring (declarations, parameters, assignment,
    catch), with the iterator protocol for arrays. needs: 1.12b, 1.8b
  - [ ] 1.12d The arguments object (mapped and unmapped), sloppy `this`.
    needs: 1.7, 1.8c
  - [ ] 1.12e Classes: constructors, methods, fields, `super`.
    needs: 1.7, 1.8e
  - [ ] 1.12f Generators as state machines. needs: 1.7, 1.12c
  - [ ] 1.12g Async functions and await. needs: 1.12f, 1.14
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
build target. Gate: 100% of Test262 for the current ECMAScript edition,
before any JIT work starts (John, 2026-11-25, D-056).

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
