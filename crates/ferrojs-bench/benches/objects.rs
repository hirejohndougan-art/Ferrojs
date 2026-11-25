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
use ferrojs_vm::{string, Got, JsString, Object, RootedValue, Vm, VmOptions};
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
            let v = match object::get(nogc, black_box(child.get(nogc)), k) {
                Got::Value(v) | Got::Getter(v) => v,
            };
            acc = acc.wrapping_add(v.to_bits());
        }
        for i in 0..1000 {
            let v = RootedValue::Primitive(Value::from_i32(i));
            object::set(s, child, keys[8 + (i as usize % 8)], v).unwrap();
        }
        acc
    })
}

/// Large objects (task 1.8f, D-060): one with 100 properties on shared
/// shapes (a key table) and one in dictionary mode with 300, and the keys.
#[cfg(target_os = "linux")]
type LargeSetup = (Vm, Global<Object>, Global<Object>, Vec<Global<JsString>>);

#[cfg(target_os = "linux")]
fn large_objects() -> LargeSetup {
    let mut vm = quiet_vm();
    let (shared, dict, keys) = vm.with(|s, t| {
        let keys: Vec<_> = (0..300)
            .map(|i| {
                let k = string::atom(s, t, &format!("k{i}")).unwrap().bind(s.nogc());
                s.root(k)
            })
            .collect();
        let mut objs = Vec::new();
        for n in [100, 300] {
            let o = object::new_object(s, t, None, DEFAULT_INLINE)
                .unwrap()
                .bind(s.nogc());
            let o = s.root(o);
            for (i, &k) in keys.iter().take(n).enumerate() {
                let v = RootedValue::Primitive(Value::from_i32(i as i32));
                object::set(s, o, k, v).unwrap();
            }
            objs.push(o);
        }
        assert!(!object::is_dictionary(s.nogc(), objs[0].get(s.nogc())));
        assert!(object::is_dictionary(s.nogc(), objs[1].get(s.nogc())));
        let nogc = s.nogc();
        let keys: Vec<_> = keys.iter().map(|k| nogc.global(k.get(nogc))).collect();
        (
            nogc.global(objs[0].get(nogc)),
            nogc.global(objs[1].get(nogc)),
            keys,
        )
    });
    (vm, shared, dict, keys)
}

// 1,000 reads of the shared-shape object's keys, then 1,000 of the
// dictionary object's.
#[cfg(target_os = "linux")]
#[library_benchmark]
#[bench::large(setup = large_objects)]
fn get_large(setup: LargeSetup) -> u64 {
    work_large(black_box(setup))
}

#[cfg(target_os = "linux")]
#[inline(never)]
fn work_large((mut vm, shared, dict, keys): LargeSetup) -> u64 {
    vm.with(|s, _| {
        let nogc = s.nogc();
        let (shared, dict) = (shared.get(nogc), dict.get(nogc));
        let keys: Vec<_> = keys.iter().map(|k| k.get(nogc)).collect();
        let mut acc = 0u64;
        for (o, n) in [(shared, 100), (dict, 300)] {
            for i in 0..1000 {
                // A stride that visits every key in a scattered order.
                let k = keys[(i * 37) % n];
                let v = match object::get(nogc, black_box(o), k) {
                    Got::Value(v) | Got::Getter(v) => v,
                };
                acc = acc.wrapping_add(v.to_bits());
            }
        }
        acc
    })
}

#[cfg(target_os = "linux")]
library_benchmark_group!(name = objects, benchmarks = [get_set, get_large]);

#[cfg(target_os = "linux")]
main!(library_benchmark_groups = objects);

#[cfg(not(target_os = "linux"))]
fn main() {}
