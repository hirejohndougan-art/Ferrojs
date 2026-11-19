# Differential test corpus

Each `.js` file here is a plain script that reports what it computes with
`print(...)`. `jsdiff` (crate `ferrojs-diff`, decision D-013) runs every file
on FerroJS and on Node.js and fails if the printed lines or the uncaught
error (constructor name and phase) differ.

Rules for files:

- Print results with `print(value)`: exactly one argument, which the host
  converts with `String`. Join several values first.
- Be deterministic: no `Math.random`, dates, timers, or anything that
  depends on the machine. The gate runs Node against a second Node process
  to catch nondeterminism.
- Avoid output whose exact form is implementation-defined (error messages,
  `Function.prototype.toString` of built-ins, property order of exotic
  objects). Print `e.name` or `e instanceof X`, not `e.message`.
- Only files under `errors/` may end with an uncaught error (the tests check
  this). Every other file must run to completion and print something.
- Group files by area in subdirectories. Add a file whenever a bug is fixed
  in an area this corpus can reach.

Run it:

```
cargo run --release -p ferrojs-diff -- --engine ferrojs tests/diff
cargo run --release -p ferrojs-diff -- --engine node tests/diff   # self-check
```
