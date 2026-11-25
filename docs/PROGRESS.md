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

## 2026-11-21 11:20 UTC — 1.5 Parser integration
- Result: merged
- Changed: new `ferrojs-compiler` crate: oxc 0.158 parser (regex early
  errors on) + `oxc_semantic` syntax checks + our early-error pass (empty:
  oxc misses nothing Test262 checks at this pin). The Test262 `ferrojs`
  engine now parses every run; negative parse-phase tests pass, the rest
  is "not run". New `parse` fuzz target and `front_end` benchmark. Fixed
  a gate hole: the runner wrote the baseline before reading it, so the
  Test262 regression check compared each run with itself (verified it now
  fails on an injected regression). Not risky, `ferrojs-core` unchanged,
  so no independent review or CI Miri needed.
- Decisions: D-032
- Test262: 8699 (+8699; first real baseline, parse-phase only)
- Next: 1.8 objects and shapes (risky? touches core heap cells) or 1.6
  bytecode compiler part 1. Tomorrow is Sunday: the weekly report is due
  first.

## 2026-11-21 13:30 UTC — 1.6a Bytecode compiler, part 1a (1.6 split into 1.6a–c)
- Result: merged
- Changed: `ferrojs_compiler::codegen` compiles function bodies with
  parameters and uncaptured locals in registers (numbers, booleans,
  null/undefined; all unary/binary/logical/conditional/assignment/update
  operators; if, loops, labels, break/continue, return, throw); anything
  else is a clean "unsupported". 22 new ops; number semantics shared in
  `ferrojs_bytecode::number`. Tests compare ~90 compiled calls bit-exact
  with Node.js; the `parse` fuzz target also compiles, verifies and runs.
  Bench baseline rewritten: `decode` +10%, `dispatch` +6% from the larger
  op list (D-033). Started at John's request ("keep going").
- Decisions: D-033
- Test262: 8699 (+0; the runner doesn't execute code until 1.10)
- Next: 1.6b nested functions, calls, environments, TDZ marker.

## 2026-11-21 14:30 UTC — 1.6b TDZ, const assignment, switch (1.6b–d re-split)
- Result: merged
- Changed: `Value::UNINITIALIZED` marker; ops `LoadUninitialized`,
  `CheckInitialized`, `ThrowConstAssign`; the compiler checks the TDZ at
  run time only for bindings with an early use or declared in a `switch`
  case block, throws `TypeError` on `const` assignment in spec order, and
  compiles `switch`. 30 more Node.js-checked cases. Remaining 1.6 work
  re-split: 1.6c try/catch/finally, 1.6d functions, closures, globals.
- Decisions: D-034
- Test262: 8699 (+0; the runner doesn't execute code until 1.10)
- Next: 1.6c try/catch/finally.

## 2026-11-21 15:20 UTC — 1.6c try/catch/finally
- Result: merged
- Changed: exception tables (`Handler`, `verify_handlers`: forward-only
  targets), `Assembler::finish_with_labels`, toy `run_with_handlers` with
  uncatchable termination. The compiler handles `try`/`catch`/`finally`
  with inline finally blocks and a completion-kind register, so
  `return`/`break`/`continue` run every enclosing finally once. 23 more
  Node.js-checked cases.
- Decisions: D-035
- Test262: 8699 (+0; the runner doesn't execute code until 1.10)
- Next: 1.6d functions, calls, closures, globals, strings. It needs
  function objects and calls in some form before the interpreter (1.7):
  consider splitting it, or doing 1.8 objects and shapes first.

## 2026-11-21 17:00 UTC — 1.6d Functions, closures, calls (1.6d/e re-split)
- Result: merged
- Changed: nested functions (hoisted declarations, expressions, arrows)
  compile into a template tree (`Func` operand, `CreateClosure`,
  `LoadClosure`); captured bindings live in per-scope environments
  (`PushEnv`/`PopEnv`/`CopyEnv`, `GetEnv`/`SetEnv` for handlers);
  `Call`/`TailCall` return `Flow::Call` to the dispatch loop; proper
  tail calls in strict tail positions. The toy context runs whole
  programs with frames, closures and environments. 32 more Node.js-
  checked cases plus a 100,000-deep tail recursion. Bench baseline
  rewritten (`decode` +12%, toy `dispatch` +29% from 54 ops; 1.7 must
  design dispatch so it doesn't scale with op count, D-036). 1.6e (script
  top level, globals, strings, `this`) now waits for 1.8 objects.
- Decisions: D-036
- Test262: 8699 (+0; the runner doesn't execute code until 1.10)
- Next: 1.7 interpreter loop (needs 1.3b, 1.6d: both done), or 1.8
  objects and shapes.

## 2026-11-21 17:20 UTC — planning: 1.7 split
- Result: review
- Changed: roadmap only. Planning 1.7 showed the interpreter can't read or
  write cells referenced from registers without a core API (`Gc` handles
  can only come from allocation, roots or cell fields), so 1.7a adds it as
  a risky core task. That needs a CI Miri run, which this session (a
  scheduled run continued at John's request) has no tool to check, so it
  stopped here. 1.7 records the design worked out for the VM.
- Decisions: none
- Test262: 8699 (+0)
- Next: 1.7a core handle API (risky; independent review and CI Miri).

## 2026-11-23 08:40 UTC — 1.7a Core handle API (partial)
- Result: partial (branch auto/2026-11-23-core-handles)
- Changed: `NoGc::handle(offset) -> Option<Gc>`, always checked against
  live cell starts, with tests and compile-fail rule 7. Independent
  review: no soundness or rooting problems; one doc misplacement and
  three test gaps (large tails, unused and free chunks, reuse), all
  fixed. Local gate passes. Not merged: Miri must pass in CI, and every
  CI run since 2026-11-21 15:49 UTC fails in about 3 seconds with no runner
  assigned (runs 51–56), which points to exhausted Actions minutes. John
  has been asked. Also wrote the first weekly report (2026-W47).
- Decisions: D-037 (on the branch)
- Test262: 8699 (+0)
- Next: merge 1.7a once CI runs (record the run id); meanwhile 1.8.

## 2026-11-23 09:40 UTC — 1.8a Objects and shapes, part a (1.8 split a–c)
- Result: merged
- Changed: new `ferrojs-vm` crate: cell tracer, string cells and
  interned atoms, shape transition trees with the prototype, ordinary
  objects with inline and out-of-line slots, data-property
  get/set/define/own_keys. Model-checked (44 random runs, 4 under GC
  stress, heap verifier and poisoning on); tracer tests fail if value or
  prototype edges are dropped (checked by mutation). New
  `objects/get_set` benchmark (3.0M instructions, about 1,500 per access
  before inline caches). Independent review: `define` let a frozen
  property's value change (fixed, SameValue) and the tests didn't cover
  cells reachable only through slots (added). No `ferrojs-core` change.
- Decisions: D-038
- Test262: 8699 (+0)
- Next: 1.8b (delete, dictionary mode, accessors) or 1.8c (elements);
  1.7a still waits for CI.

## 2026-11-23 10:30 UTC — 1.8b Delete, attributes, dictionary mode
- Result: merged
- Changed: dictionary-mode shapes with an ordered entry table (slot
  reuse, compaction); `define` per ValidateAndApplyPropertyDescriptor,
  `delete`, `prevent_extensions`/`is_extensible`; 128-property limit for
  shared shapes. Faster shared-shape lookup: `objects/get_set` 2.94M ->
  2.36M instructions (-20%; baseline rewritten). Model test now covers
  delete, attribute changes and non-extensible objects with 6 and 200
  keys, two runs under GC stress. Independent review: one must-fix
  (dictionary entry recorded before its storage existed; fixed), test
  gaps addressed. Accessors and the lazy key table split to 1.8d.
- Decisions: D-039
- Test262: 8699 (+0)
- Next: 1.8c elements (indexed properties). 1.7a still waits for CI.

## 2026-11-23 11:15 UTC — 1.8c Indexed properties and arrays
- Result: merged
- Changed: `ferrojs_vm::elements`: dense (hole markers) or sparse
  stores, arrays with length in the store, get/set/delete/keys/
  set_length, prototype fall-through for holes. Model-checked (30 runs,
  3 under GC stress); element-liveness test verified by mutation.
  Independent review: no must-fix; nice-to-haves applied (tracer hole
  exemption only for elements, doc wording, a length-growth test).
- Decisions: D-040
- Test262: 8699 (+0)
- Next: 1.11 host hooks (independent), or 1.7a once CI runs again.

## 2026-11-23 11:35 UTC — 1.11 Host hooks
- Result: merged
- Changed: `ferrojs_vm::host`: the `Host` trait (random, now,
  local_offset, monotonic_now, enqueue_job/next_job), `SystemHost`,
  `DeterministicHost`, xorshift128+; the `Vm` owns a host. Tests for
  range and spread of random numbers, reproducibility, clocks and job
  order (including a custom LIFO host). Not risky; no review needed.
- Decisions: D-041
- Test262: 8699 (+0)
- Next: 1.7a once CI runs (everything else open needs 1.7 or 1.6e,
  and 1.6e needs strings and globals from the VM side).

## 2026-11-23 12:40 UTC — 1.6e Scripts and globals (1.6e split e–g)
- Result: merged
- Changed: `compile_script`; globals and undeclared names by name
  (`LoadName`, `StoreName`, `StoreNameStrict`, `DeclareGlobal`,
  `InitGlobal`), `LoadThis` in strict functions, string constants. The
  toy context models the global lexical environment and global object.
  31 multi-script cases match Node.js (including cross-script
  redeclaration SyntaxErrors and TDZ). Gate fix: `objects/get_set` had
  measured nothing since 1.11 (5,776 instead of 2.36M); fixed, and the
  benchmark check now fails on drops over 50% (D-043).
- Decisions: D-042, D-043
- Test262: 8699 (+0)
- Next: 1.6f strings (literals, typeof, concatenation, Number::toString).

## 2026-11-23 09:25 UTC — 1.6f Strings
- Result: merged
- Changed: string literals and template literals compile to string
  constants, `ToString` and `Add`; `typeof` (`Typeof`, `TypeofName`).
  The toy context implements concatenation, Number::toString,
  StringToNumber, code-unit comparison, loose equality and ToBoolean for
  strings. 34 new script cases match Node.js, compared as UTF-16 code
  units; a mutation (UTF-8 instead of UTF-16 order) is caught. Not
  risky (no core, unsafe or GC code); no review needed.
- Decisions: D-044
- Test262: 8699 (+0)
- Next: 1.6g (Annex B call-expression assignment targets), or 1.7a
  once CI runs again.

## 2026-11-23 09:32 UTC — 1.6g Sloppy-mode calls as assignment targets
- Result: merged
- Changed: front end rewrites `f() = e`, `f() op= e`, `f()++`, `--f()`
  and `for (f() in/of …)` in sloppy scripts so oxc accepts them, then
  validates each site (strict, logical assignment, destructuring,
  `super()`, optional and non-call targets stay SyntaxErrors); offsets
  map back to the source. The compiler runs the call, then throws a
  `ReferenceError` (`ThrowInvalidAssignment`). 21 run cases match
  Node.js; validation rules checked by mutation. Not risky (no core,
  unsafe or GC code); no review needed.
- Decisions: D-045
- Test262: 8699 (+0); failures 7 -> 0 (the 7 Annex B runs now parse and
  are "not run" until the interpreter is hooked up)
- Next: 1.7a once CI runs again (everything else open needs 1.7).

## 2026-11-23 11:50 UTC — 1.12a Default parameter values (1.12 split a–g)
- Result: merged
- Changed: parameter initializers compile (left to right, `undefined`
  triggers the default); parameters reachable early get a TDZ binding
  checked everywhere, closures included; `CompiledFunction::length`.
  28 new script cases match Node.js; mutations (no TDZ, no env store
  for captured params, no TDZ check in nested functions) are caught. An
  unresolved `arguments` in a function is now refused instead of
  compiling as a global. Not risky (no core, unsafe or GC code); no
  review needed. 1.12 split: the rest needs objects or the interpreter.
- Decisions: D-046
- Test262: 8699 (+0)
- Next: 1.7a once CI runs again. GitHub Actions still starts no job
  (every run fails in ~3 s since 2026-11-21) and rustup can't reach
  static.rust-lang.org here, so Miri can't run anywhere; every other
  open task waits on 1.7. John has been notified.

## 2026-11-24 08:40 UTC — 1.7c Strings and globals in the interpreter
- Result: done on branch `auto/2026-11-24-interp-strings` (stacked on
  1.7b and 1.7a), not merged: 1.7a changes `ferrojs-core` and still
  needs a Miri run. GitHub Actions still fails every job in ~3 s and
  rustup can't reach static.rust-lang.org here.
- Changed: run-time strings (concatenation, ToString, StringToNumber,
  code-unit comparison and equality by contents); the global object and
  global lexical environment; `Vm::run_script`, `Vm::global`. The script
  cases moved to a shared file and run on the interpreter, plain and
  under GC stress, against Node.js. Toy context fix: `var` over an
  implicit global no longer makes it non-configurable. New benchmark
  `interp/run.global_strings`. Independent review: no rooting or
  memory-safety bugs; its fixes (length checks before copying, strict
  global store cases) are in, and the spec gaps it found are recorded
  in D-050 for 1.13.
- Gate: everything passes except Miri (waits for CI).
- Decisions: D-050
- Test262: 8699 (+0)
- Next: 1.7d (dispatch cost vs op count), stacked on this branch; or
  1.7a's Miri run once CI works, then merge 1.7a → 1.7b → 1.7c.

## 2026-11-25 04:10 UTC — 1.7d Dispatch cost against the op count
- Result: done on branch `auto/2026-11-25-dispatch` (stacked on 1.7c,
  1.7b and 1.7a), not merged: 1.7a still needs a Miri run, and GitHub
  Actions still fails every job.
- Changed: the interpreter dispatches through a table of per-op step
  functions generated by `define_ops!` (one indexed call per
  instruction) instead of decode-then-execute. Interpreter benchmarks:
  `loop_with_calls` −36%, `global_strings` −9%. A `padding-ops`
  feature adds 150 unused ops; with it the old design was 9.6% slower,
  the new one 0.06%. New gate step `tools/dispatch-flat.sh` (also in
  CI) fails if 150 extra ops move any interpreter benchmark by more
  than 1%. Randomized equivalence test against decode + execute.
  Independent review: no serious findings; script fixes applied.
- Gate: everything passes except Miri (waits for CI).
- Decisions: D-052
- Test262: 8699 (+0)
- Next: 1.8d (accessors) or 1.9 (embedding API and the `ferrojs`
  binary), stacked on this branch; or merge 1.7a → 1.7d once CI runs.

## 2026-11-25 04:40 UTC — 1.7e Property access (task added this run)
- Result: done on branch `auto/2026-11-25-property-access` (stacked on
  1.7c, beside 1.7d's branch), not merged: the stack carries 1.7a's
  `ferrojs-core` change, which still needs a Miri run. GitHub Actions
  still fails every job in ~3 s; rustup still can't reach
  static.rust-lang.org.
- Changed: added task 1.7e (nothing compiled `o.x`, and 1.8d, 1.9,
  1.12b–e and 1.13 need it). 12 property ops; object literals, member
  reads, writes, compound, logical and update assignment, `delete`, `in`,
  method calls with `this`; in the compiler, the toy context and the
  interpreter (elements for indices, atoms for names). 70 Node-checked
  script cases on both engines (plus GC stress), 5 interpreter-only
  ones, refusal tests; new benchmark `interp/run.properties`.
  Independent review: no rooting or memory-safety bugs; fixed its
  findings (inherited `Object.prototype` names and `__proto__` now fail
  cleanly instead of answering wrongly; error kinds asserted; toy and
  interpreter agree on odd bytecode).
- Gate: everything passes except Miri (waits for CI).
- Decisions: D-053 (D-052 went to 1.7d)
- Test262: 8699 (+0)
- Next: once Miri runs, merge 1.7a → 1.7b → 1.7c, then 1.7d and 1.7e
  (both stacked on 1.7c and both add to the op list and
  `benches/interp.rs`: the second to merge rebases). Otherwise 1.8d or
  1.12d could stack on this branch, but the stack is getting deep.

## 2026-11-25 06:10 UTC — 1.9 Minimal embedding API and the `ferrojs` binary
- Result: done on branch `auto/2026-11-25-cli`, not merged: the stack
  under it carries 1.7a's `ferrojs-core` change, which still needs a Miri
  run. GitHub Actions still fails every job in ~3 s with no logs
  (runs up to 88); rustup still can't reach static.rust-lang.org (proxy
  refuses the tunnel).
- Changed: the branch first merges 1.7d into 1.7e (conflicts in the op
  list, bench file and baseline resolved), so the whole stack is one
  line: 1.7a → 1.7b → 1.7c → 1.7e + 1.7d → 1.9. Host functions: `Native`
  cells indexing Rust closures that only see owned `HostValue` copies;
  `Vm::define_native`, `Vm::host_value`. New `ferrojs` crate (`Engine`:
  run, register, global, interrupt handle; errors with phase, line and
  column, constructor name) and `ferrojs-cli` (`ferrojs file.js`, `-e`,
  `print`, exit codes 0/1/2/3). New fuzz target `run_script` on the real
  interpreter (3 min run: no crashes). Independent review: no serious
  findings; fixed its minor ones (host objects now a `TypeError`,
  CRLF column, slot reuse on redefinition, `--`, panic note).
  `loop_with_calls` +1% from the call path.
- Gate: everything passes except Miri (waits for CI).
- Decisions: D-054
- Test262: 8699 (+0)
- Next: 1.10 (the Test262 runner's `ferrojs` engine on the real engine,
  first real baseline) stacked on `auto/2026-11-25-cli`; or, once Miri
  can run, merge the stack in order (fast-forward of
  `auto/2026-11-25-cli` brings everything).
