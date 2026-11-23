//! Object property access (D-038): reading and writing named properties
//! of ordinary objects through shapes, own and inherited, inline and
//! out-of-line. No inline caches yet (ARCHITECTURE.md §7.3).
//!
//! Linux-only like the other benchmarks: every item is behind
//! `cfg(target_os = "linux")`, and other platforms get an empty `main`.

#[cfg(target_os = "linux")]
use std::hint::black_box;

#[cfg(target_os = "linux")]
use ferrojs_core::Global;
#[cfg(target_os = "linux")]
use ferrojs_value::Value;
#[cfg(target_os = "linux")]
use ferrojs_vm::object::{self, DEFAULT_INLINE};
#[cfg(target_os = "linux")]
use ferrojs_vm::{string, JsString, Object, RootedValue, Vm, VmOptions};
#[cfg(target_os = "linux")]
use gungraun::prelude::*;

#[cfg(target_os = "linux")]
fn quiet_vm() -> Vm {
    Vm::new(VmOptions {
        cx: ferrojs_core::CxOptions { gc_stress: false },
        heap: ferrojs_core::HeapOptions {
            poison: false,
            verify: false,
        },
        ..VmOptions::default()
    })
    .expect("a heap")
}

/// The engine with a prototype holding `p0..p7` and a child holding
/// `p8..p15` (half of them out of line), and the 16 keys.
#[cfg(target_os = "linux")]
type Setup = (Vm, Global<Object>, Vec<Global<JsString>>);

#[cfg(target_os = "linux")]
fn two_objects() -> Setup {
    let mut vm = quiet_vm();
    let (child, keys) = vm.with(|s, t| {
        let keys: Vec<_> = (0..16)
            .map(|i| {
                let k = string::atom(s, t, &format!("p{i}")).unwrap().bind(s.nogc());
                s.root(k)
            })
            .collect();
        let proto = object::new_object(s, t, None, DEFAULT_INLINE)
            .unwrap()
            .bind(s.nogc());
        let proto = s.root(proto);
        let child = object::new_object(s, t, Some(proto), DEFAULT_INLINE)
            .unwrap()
            .bind(s.nogc());
        let child = s.root(child);
        for (i, &k) in keys.iter().enumerate() {
            let o = if i < 8 { proto } else { child };
            let v = RootedValue::Primitive(Value::from_i32(i as i32));
            object::set(s, o, k, v).unwrap();
        }
        let nogc = s.nogc();
        let keys: Vec<_> = keys.iter().map(|k| nogc.global(k.get(nogc))).collect();
        (nogc.global(child.get(nogc)), keys)
    });
    (vm, child, keys)
}

// 1,000 reads spread over own and inherited keys, then 1,000 writes to
// existing own properties.
#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::two_objects(setup = two_objects)]
fn get_set(setup: Setup) -> u64 {
    // Out of line, so the measured function always contains the work
    // (inlined into the harness, it once measured only the drops).
    work(black_box(setup))
}

#[cfg(target_os = "linux")]
#[inline(never)]
fn work((mut vm, child, keys): Setup) -> u64 {
    vm.with(|s, _| {
        let child = s.root(child.get(s.nogc()));
        let keys: Vec<_> = keys.iter().map(|k| s.root(k.get(s.nogc()))).collect();
        let mut acc = 0u64;
        for i in 0..1000 {
            let nogc = s.nogc();
            let k = keys[i % 16].get(nogc);
            acc = acc.wrapping_add(object::get(nogc, black_box(child.get(nogc)), k).to_bits());
        }
        for i in 0..1000 {
            let v = RootedValue::Primitive(Value::from_i32(i));
            object::set(s, child, keys[8 + (i as usize % 8)], v).unwrap();
        }
        acc
    })
}

#[cfg(target_os = "linux")]
library_benchmark_group!(name = objects, benchmarks = [get_set]);

#[cfg(target_os = "linux")]
main!(library_benchmark_groups = objects);

#[cfg(not(target_os = "linux"))]
fn main() {}
