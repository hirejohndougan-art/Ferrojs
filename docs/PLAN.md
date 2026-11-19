# FerroJS plan

A copy of the planning document (claude.ai doc "FerroJS — Planning
Document", Nov 2026), condensed for autopilot runs. Where this file and
`docs/DECISIONS.md` disagree, the newer accepted decision wins.

## Goals

A new JavaScript engine written from scratch in 100% Rust, MIT licensed,
with a tiered JIT, aiming to compete with V8 on speed, correctness and
efficiency. V8 parity is the direction, not the first finish line.

Milestones:

1. **Usable engine:** over 95% of Test262 and runs real scripts, then 100%
   of Test262 for the current ECMAScript edition (ES2026 today), required
   for 1.0.
2. **Faster than QuickJS and Boa,** via a baseline JIT with inline caches.
3. **Instant warm starts,** via saved type profiles and a bytecode cache,
   ahead of V8 (with its code cache) on repeat runs.
4. **Competitive peak speed** with V8 on a broad benchmark set, via an
   optimizing tier and a generational moving GC.

## Guiding principles

- **100% Rust.** No C or C++ dependencies. `unsafe` and a little inline
  assembly only in a small, audited core.
- **100% accurate on the current edition.** Unfinished proposals stay
  behind a flag.
- **Safety by design:** safe Rust outside the core, a sandboxed heap, JIT
  checked against the interpreter.
- **Build in stages.** The interpreter is always the complete, correct
  implementation; each JIT tier adds speed with a fallback. Every phase
  ships a working engine.
- **Built for handoff:** architecture guide, decision log, documented
  invariants and debugging tools, written as the code is written.
- **Reuse the spec busywork, own the speed.** Existing crates for parsing,
  regex, Temporal and Intl; our own object model, GC and JIT.

## Prior art to study (ideas only, never copy code)

SpiderMonkey (CacheIR, Warp), LuaJIT Remake (interpreter and copy-and-patch
baseline generated from one set of op definitions), Kataan (Rust, closest
design), Zipp (Rust, high Test262), Boa (Rust, 96.1% Test262, no JIT),
GraalJS (interpreter-derived JIT), Hermes, CPython 3.13 copy-and-patch JIT,
and the copy-and-patch paper.

## Architecture

All code starts in the interpreter; hot code moves up to faster tiers;
every tier can fall back to the interpreter. Only the bottom layer uses
`unsafe`.

- **Values:** NaN-boxed 64-bit values (D-001).
- **Heap:** one caged region per engine instance, 4 GB by default,
  references as 32-bit offsets (D-002).
- **GC:** precise and generational: a bump-allocated nursery that copies
  survivors and a mark-compact old generation. Built to allow moving from
  day one even though Phase 1 ships a simple non-moving collector. Rust
  code reaches heap objects only through lifetime-branded handles
  (`gc-arena` style), so rooting mistakes fail at compile time.
- **Objects and inline caches:** hidden-class shapes from day one. Every
  inline cache records fast paths in one shared format (CacheIR model)
  used by all tiers.
- **Single op definitions:** each bytecode op is defined once in a Rust
  macro language that generates its interpreter handler, its
  copy-and-patch stencil and its optimizer lowering.
- **Optimizing tier:** Warp-style SSA compiler built from inline-cache data
  (D-003). E-graph passes are a later option.
- **Frames and bytecode:** register-based bytecode; interpreter and
  baseline JIT share one frame layout; proper tail calls from the start.
- **Concurrency:** one engine instance per thread with its own heap.
  SharedArrayBuffer memory lives outside the GC heap; Atomics and
  multi-agent tests run through host hooks. Background JIT compilation and
  concurrent marking in Phase 5.
- **Platforms:** x86-64 and ARM64 on Linux, macOS and Windows; a no-JIT
  mode for iOS and WebAssembly hosts.
- **WebAssembly:** full JS API; modules validated with `wasmparser` and
  compiled with Cranelift (D-010); Wasm memory in its own sandboxed region
  with guard pages.

## New approaches

| Technique | Goal |
| --- | --- |
| Spec-generated runtime: built-ins generated from ECMA-262's structured pseudocode (ESMeta style), hand-tuned hot paths | Accuracy |
| Single op definitions + shared inline-cache format | Tiers can't disagree |
| Copy-and-patch baseline (stencils written in Rust, compiled by rustc); Cranelift as fallback | Near-instant compiles |
| Saved type profiles + bytecode cache (never machine code) | No warm-up on repeat runs |

Optional later: TypeScript types as a head start, a tracing JIT for hot
loops, e-graph optimization. Truffle-style partial evaluation was rejected
(heavy, hard to debug).

## Safety design

1. `unsafe` isolated in `ferrojs-core`; everything else forbids it (D-004).
2. GC safety through branded handles; raw offsets only in the core and JIT code.
3. Pointer cage: buggy JIT code can't reach memory outside the JS heap.
4. Typed IR: values can't be used as integers without a guard.
5. Verification: Miri and Kani on the core; fuzzing; differential testing
   of every JIT tier against the interpreter.
6. Hardening: W^X memory, control-flow integrity, Spectre mitigations.
7. Cache files are untrusted input (D-006): bytecode and profiles only,
   with build hash and checksum.

## Quality and testing system

Built first, before the interpreter.

- **Self-checks (debug builds):** invariant assertions, heap verifier after
  each GC, IR verifier after each pass, stress switches (GC on every
  allocation, JIT everything, random deopt), shadow mode comparing
  interpreter and JIT results live.
- **Pipeline:** every change runs build, unit tests, Test262 and
  differential checks; nothing merges on failure. Nightly: full Test262
  under stress modes, timing benchmarks, Miri and sanitizers. Continuous:
  Fuzzilli and cargo-fuzz against the interpreter and V8.
- **Bug-to-test loop:** failure → minimize → inspect with dump tools → fix
  lands with the minimized program as a regression test → dashboard.

### Accuracy target

100% of Test262 for the current edition plus Annex B. Intl (ECMA-402)
reported separately; Stage 3 proposals and `staging` excluded. New
features: behind a flag at Stage 3, on by default at Stage 4. Verified by
spec-generated built-ins, spec-step coverage, a three-engine vote (FerroJS,
V8, SpiderMonkey) on fuzzed programs, and spec-generated tests. Hardest
item: proper tail calls.

### Performance tracking

JetStream as the main suite, Octane and SunSpider for comparison;
real-world workloads (TypeScript compiler, Babel, a bundler, JSON, regex);
micro-benchmarks per feature; startup, memory, GC pauses, compile time.
Every change: `iai-callgrind` instruction counts. Nightly: real timing with
automatic bisection. Weekly: compare with V8 (full and jitless), QuickJS
and Boa.

Targets: Phase 1 interpreter within 2× QuickJS; Phase 3 faster than
QuickJS and Boa; Phase 4 repeat-run startup ahead of V8 with code cache;
Phase 5 steady climb toward V8 parity.

## Phased roadmap

1. **Core engine (1–2 months):** op-definition language, bytecode compiler
   on oxc, interpreter, shapes, simple GC behind branded handles, basic
   built-ins, minimal embedding API. Runs real scripts.
2. **Full language and built-ins (3–6 months):** spec generation; over 95%
   of Test262. Biggest unknown.
3. **Baseline JIT and inline caches (1–2 months).**
4. **Persistent profiles, code caching, embedding API 1.0 (weeks).**
5. **Optimizing tier and generational moving GC (6–12+ months).** Riskiest.
6. **WebAssembly (3–6 months, alongside 5).**

Closing the gap to 100% Test262, including tail calls, runs alongside
Phases 3–4 and gates 1.0 (roughly 9–12 months in). Regex JIT in Phase 5.
Size: about 150,000–300,000 lines of Rust, roughly half built-ins.

## Adoption features (1.0 requires 1–5, 7 and 8)

1. Rust embedding API: derive macro, Promise/async (Tokio) bridge.
   Minimal in 1; macros and async in 3; frozen in 4.
2. Sandboxing: memory caps, CPU limits, interrupts, permission model.
   Limits in 1–2; permissions in 4.
3. Startup snapshots (4).
4. Built-in TypeScript via oxc type stripping (2).
5. Chrome DevTools Protocol debugging: hooks in 2, protocol in 4, JIT
   breakpoints in 5.
6. Optional `ferrojs-web` crate with WinterTC APIs (4 onward).
7. C API (4); other bindings after 1.0.
8. Trust signals: public dashboards (from 1), security policy and OSS-Fuzz
   (before first public release, 3), semver and migration guides (1.0).
9. Deterministic mode (4).
10. FerroJS compiled to WebAssembly: build target from 2, CI from 3.

## Licensing and dependencies

| Dependency | Role | License |
| --- | --- | --- |
| Cranelift, wasmparser | Wasm compiler, backup baseline backend | Apache-2.0 with LLVM exception |
| oxc | Parser | MIT |
| regress | Regex engine | MIT or Apache-2.0 |
| temporal_rs | Temporal | MIT or Apache-2.0 |
| ICU4X | Intl | Unicode License v3 |
| Test262 | Tests (not shipped) | BSD-3-Clause |

Read other engines for ideas only; copied MPL-2.0 code would stay MPL, so
nothing is copied. Do a patent search before any commercial release.

## Adoption strategy

Win the Rust embedding niche first (Rust apps, serverless and edge cold
starts, plugin sandboxes, games; possibly Servo). Edge beyond speed: pure
Rust, memory safety, instant warm starts, small binary, easy embedding.

## Key risks and fallbacks

| Risk | Fallback |
| --- | --- |
| Optimizing tier and moving GC take far longer | Ship value at each earlier phase |
| Spec generation is harder than expected | Hand-written built-ins for the gaps |
| Parser misses required syntax errors | Own early-error pass; upstream fixes to oxc; switch to Boa's parser if the gap is large |
| Rust copy-and-patch stencils don't work cleanly | Cranelift baseline tier |
| Subtle JIT miscompiles | Differential testing, fuzzing, pointer cage |
| Wasm GC strains the GC design | Core Wasm first; reserve room in the heap design |
| Regex speed lags | Regex JIT in Phase 5 |
| 4 GB cage too small | Large-heap mode later |
| Context loss between sessions | This file, the decision log, the roadmap and the progress log |
