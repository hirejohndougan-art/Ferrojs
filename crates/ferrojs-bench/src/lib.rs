//! Instruction-count benchmarks for FerroJS (decision D-015).
//!
//! The benchmarks live in `benches/` and run under Valgrind through
//! gungraun. This library holds the deterministic inputs they share and the
//! code behind the `benchcheck` tool, which compares the counts with the
//! stored baseline in `conformance/bench-baseline.txt`.

pub mod baseline;
pub mod inputs;
