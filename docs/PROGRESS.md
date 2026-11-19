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
  gate step and CI job (fails on >3% more instructions).
- Decisions: D-015
- Test262: 0 (+0; engine can't run JS yet)
- Next: 0.4 Miri job on ferrojs-core
