# FerroJS

A JavaScript engine in 100% Rust, aiming for 100% Test262 accuracy on the
current ECMAScript edition and V8-class speed through a tiered JIT. MIT licensed.

Status: **early development.** The value representation and the Test262
conformance runner exist; the engine cannot run JavaScript yet.

## Layout

| Path | What it is |
| --- | --- |
| `crates/ferrojs-value` | NaN-boxed 64-bit JavaScript values (safe Rust) |
| `crates/ferrojs-core` | The only crate allowed to use `unsafe` (empty for now) |
| `crates/ferrojs-test262` | Test262 conformance runner and regression gate |
| `conformance/test262-baseline.txt` | Every Test262 run that currently passes |
| `tools/test262-commit.txt` | The Test262 commit all results are measured against |
| `tools/fetch-test262.sh` | Downloads Test262 at that commit |
| `docs/DECISIONS.md` | Decision log: read at the start of every session |
| `docs/AUTOPILOT.md` | How scheduled Claude sessions build FerroJS without a human in the loop |
| `docs/ROADMAP.md` | Task queue the autopilot works through |
| `docs/PROGRESS.md` | One entry per autopilot run |
| `docs/PLAN.md` | Goals, architecture and phases |
| `.github/workflows/ci.yml` | Build, lint and test on Linux, macOS and Windows, then the Test262 gate |

## Rules

- `unsafe` code is forbidden outside `ferrojs-core`; the compiler enforces it.
- Every change passes `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test` and the Test262 gate before it lands.
- A test that passed before must keep passing. When a change makes new tests
  pass, refresh the baseline in the same commit.
- Every bug fix lands with a test that reproduces it.

## Run

Unit tests:

```
cargo test --release
```

Test262 (about 103,000 test runs):

```
tools/fetch-test262.sh
cargo run --release -p ferrojs-test262 -- --suite test262 --baseline conformance/test262-baseline.txt
```

Useful options: `--filter built-ins/Array` to run part of the suite,
`--show-failures 20` to list failures, `--out report.json` for the full report,
`--write-baseline conformance/test262-baseline.txt` to record new passes.

`--engine node` runs the same suite on Node.js. It's a reference engine: it
checks that the runner itself is correct, and it is one of the engines FerroJS
results are compared against.
