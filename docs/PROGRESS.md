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
