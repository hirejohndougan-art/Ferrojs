//! Front-end benchmark (D-018): parsing, scope analysis and early-error
//! checks for a generated script, the path every script takes before it
//! runs (task 1.5).
//!
//! Linux-only like the other benchmarks: every item is behind
//! `cfg(target_os = "linux")`, and other platforms get an empty `main`.

#[cfg(target_os = "linux")]
use std::hint::black_box;

#[cfg(target_os = "linux")]
use ferrojs_compiler::{parse, Goal};
#[cfg(target_os = "linux")]
use gungraun::prelude::*;

/// About 200 lines of ordinary code: functions, closures, classes, loops,
/// object and array literals, templates and destructuring.
#[cfg(target_os = "linux")]
fn mixed_script() -> String {
    let mut s = String::from("'use strict';\n");
    for i in 0..20 {
        s.push_str(&format!(
            "function f{i}(a, b = {i}, ...rest) {{\n\
             \x20 let total = 0;\n\
             \x20 for (let k = 0; k < a.length; k++) {{ total += a[k] * b; }}\n\
             \x20 const {{ x, y: [p, q] }} = {{ x: total, y: [1, 2] }};\n\
             \x20 return `${{x + p + q}}:${{rest.length}}`;\n\
             }}\n\
             class C{i} extends Object {{\n\
             \x20 #v = {i};\n\
             \x20 get v() {{ return this.#v; }}\n\
             \x20 static make() {{ return new C{i}(); }}\n\
             }}\n\
             const g{i} = (n) => n > 1 ? g{i}(n - 1) + n : 1;\n"
        ));
    }
    s
}

#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::mixed(setup = mixed_script)]
fn front_end(source: String) -> bool {
    let allocator = ferrojs_compiler::Allocator::default();
    parse(&allocator, black_box(&source), Goal::Script).is_ok()
}

#[cfg(target_os = "linux")]
library_benchmark_group!(name = parse_group, benchmarks = [front_end]);

#[cfg(target_os = "linux")]
main!(library_benchmark_groups = parse_group);

#[cfg(not(target_os = "linux"))]
fn main() {}
