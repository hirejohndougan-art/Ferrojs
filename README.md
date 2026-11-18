# rjs (working name)

A JavaScript engine in 100% Rust, aiming for 100% Test262 accuracy on the
current ECMAScript edition and V8-class speed through a tiered JIT. MIT licensed.

Status: **session 1, project skeleton.** Not usable yet.

## Layout

| Path | What it is |
| --- | --- |
| `crates/rjs_value` | NaN-boxed 64-bit JavaScript values (safe Rust) |
| `crates/rjs_core` | The only crate allowed to use `unsafe` (empty for now) |
| `docs/DECISIONS.md` | Decision log: read at the start of every session |
| `.github/workflows/ci.yml` | Build, lint and test on Linux, macOS and Windows |

## Rules

- `unsafe` code is forbidden outside `rjs_core`; the compiler enforces it.
- Every change passes `cargo fmt --check`, `cargo clippy -D warnings` and
  `cargo test` before it lands.
- Every bug fix lands with a test that reproduces it.

## Run

```
cargo test --release
```
