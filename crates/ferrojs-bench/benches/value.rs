//! Value encoding micro-benchmarks (D-001). Each benchmark processes
//! `inputs::N` values so per-call costs show up clearly in the counts.

use std::hint::black_box;

use ferrojs_bench::inputs;
use ferrojs_value::{Kind, Value};
use gungraun::prelude::*;

#[library_benchmark]
#[bench::mixed(setup = inputs::numbers)]
fn from_f64(numbers: Vec<f64>) -> u64 {
    let mut acc = 0u64;
    for &d in &numbers {
        acc = acc.wrapping_add(Value::from_f64(black_box(d)).to_bits());
    }
    acc
}

#[library_benchmark]
#[bench::mixed(setup = inputs::numbers)]
fn number(numbers: Vec<f64>) -> u64 {
    let mut acc = 0u64;
    for &d in &numbers {
        acc = acc.wrapping_add(Value::number(black_box(d)).to_bits());
    }
    acc
}

#[library_benchmark]
#[bench::mixed(setup = inputs::bit_patterns)]
fn from_bits_checked(bits: Vec<u64>) -> usize {
    bits.iter()
        .filter(|&&b| Value::from_bits_checked(black_box(b)).is_some())
        .count()
}

fn valid_values() -> Vec<Value> {
    inputs::bit_patterns()
        .into_iter()
        .filter_map(Value::from_bits_checked)
        .collect()
}

#[library_benchmark]
#[bench::mixed(setup = valid_values)]
fn kind(values: Vec<Value>) -> u64 {
    let mut acc = 0u64;
    for &v in &values {
        acc = acc.wrapping_add(match black_box(v).kind() {
            Kind::Int32(i) => i as u64,
            Kind::Double(d) => d.to_bits(),
            Kind::Bool(b) => b as u64,
            Kind::Object(r) | Kind::String(r) | Kind::Other(_, r) => r.0 as u64,
            Kind::Undefined | Kind::Null => 1,
        });
    }
    acc
}

#[library_benchmark]
#[bench::mixed(setup = valid_values)]
fn as_number(values: Vec<Value>) -> f64 {
    let mut acc = 0.0;
    for &v in &values {
        if let Some(d) = black_box(v).as_number() {
            acc += d;
        }
    }
    acc
}

library_benchmark_group!(
    name = value_encoding,
    benchmarks = [from_f64, number, from_bits_checked, kind, as_number]
);

main!(library_benchmark_groups = value_encoding);
