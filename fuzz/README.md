# Fuzz targets

libFuzzer targets for FerroJS (decision D-014). They build on stable Rust
through `tools/fuzz.sh`; no nightly or `cargo-fuzz` install is needed.

| Target | What it checks |
| --- | --- |
| `value_bits` | Any 64 bits through `Value::from_bits_checked`: accepted bits decode and re-encode to the same bits |
| `bytecode_verify` | Any bytes through the bytecode verifier: no panics; accepted code disassembles, re-encodes to the same instructions, and runs on the toy context without panicking |
| `value_roundtrip` | Every constructor's output is accepted by the checked decoder and decodes to what went in; `Value::number` keeps value, sign of zero and the right int32/double form |

## Running

```
tools/fuzz.sh smoke            # every target, 10 s each (the gate)
tools/fuzz.sh run value_bits   # one target until Ctrl-C, keeps fuzz/corpus/value_bits/
tools/fuzz.sh run value_bits 3600
```

For AddressSanitizer (useful once `ferrojs-core` has `unsafe` code):

```
cd fuzz && cargo +nightly fuzz run value_bits
```

A crash leaves its input in `fuzz/artifacts/<target>/`. Reproduce it with
`fuzz/target/<host>/release/<target> <file>`, fix the bug, and add a unit
test with that input to the crate that had the bug.

## Adding a target

Add `fuzz_targets/<name>.rs` and a `[[bin]]` entry in `Cargo.toml`. Assert
properties (round trips, agreement with a reference), not just "no panic".
