//! Indexed properties and arrays (task 1.8c, D-040), checked against a
//! Rust model with the heap verifier on, and under GC stress.

use std::collections::BTreeMap;

use ferrojs_core::gc::{CxOptions, Root};
use ferrojs_core::heap::HeapOptions;
use ferrojs_value::{HeapRef, Value};
use ferrojs_vm::object::{self, DEFAULT_INLINE};
use ferrojs_vm::{elements, string, Object, RootedValue, Vm, VmOptions};

fn vm(stress: bool) -> Vm {
    Vm::new(VmOptions {
        heap_size: 96 * ferrojs_core::cage::CHUNK_SIZE as u64,
        heap: HeapOptions {
            poison: true,
            verify: true,
        },
        cx: CxOptions { gc_stress: stress },
    })
    .unwrap()
}

#[test]
fn arrays_track_their_length() {
    let mut vm = vm(true);
    vm.with(|s, t| {
        let a = elements::new_array(s, t, None).unwrap().bind(s.nogc());
        let a = s.root(a);
        assert!(elements::is_array(s.nogc(), a.get(s.nogc())));
        assert_eq!(elements::length(s.nogc(), a.get(s.nogc())), 0);
        for i in 0..20 {
            elements::set(s, a, i, RootedValue::Primitive(Value::from_i32(i as i32))).unwrap();
        }
        assert_eq!(elements::length(s.nogc(), a.get(s.nogc())), 20);
        // A far index makes the store sparse and the length follows.
        elements::set(s, a, 5_000_000, RootedValue::Primitive(Value::TRUE)).unwrap();
        let nogc = s.nogc();
        assert_eq!(elements::length(nogc, a.get(nogc)), 5_000_001);
        assert_eq!(elements::get(nogc, a.get(nogc), 7), Value::from_i32(7));
        assert_eq!(elements::get(nogc, a.get(nogc), 5_000_000), Value::TRUE);
        assert_eq!(elements::get(nogc, a.get(nogc), 100), Value::UNDEFINED);
        // Shrinking deletes what's past the new length; holes stay holes.
        assert!(elements::set_length(s, a, 10).unwrap());
        let nogc = s.nogc();
        assert_eq!(
            elements::keys(nogc, a.get(nogc)),
            (0..10).collect::<Vec<_>>()
        );
        assert!(elements::delete(nogc, a.get(nogc), 3));
        assert_eq!(
            elements::length(nogc, a.get(nogc)),
            10,
            "delete keeps length"
        );
        assert_eq!(elements::get_own(nogc, a.get(nogc), 3), None);
        // The largest index is 2^32 - 2.
        let max = elements::MAX_INDEX;
        assert!(elements::set(s, a, max, RootedValue::Primitive(Value::NULL)).unwrap());
        assert_eq!(elements::length(s.nogc(), a.get(s.nogc())), u32::MAX);
        assert!(!elements::set(s, a, u32::MAX, RootedValue::Primitive(Value::NULL)).unwrap());
    });
}

#[test]
fn holes_fall_through_to_the_prototype_and_non_extensible_objects_refuse() {
    let mut vm = vm(false);
    vm.with(|s, t| {
        let proto = object::new_object(s, t, None, DEFAULT_INLINE)
            .unwrap()
            .bind(s.nogc());
        let proto = s.root(proto);
        elements::set(s, proto, 1, RootedValue::Primitive(Value::from_i32(11))).unwrap();
        let a = elements::new_array(s, t, Some(proto))
            .unwrap()
            .bind(s.nogc());
        let a = s.root(a);
        elements::set(s, a, 0, RootedValue::Primitive(Value::from_i32(0))).unwrap();
        elements::set(s, a, 2, RootedValue::Primitive(Value::from_i32(2))).unwrap();
        let nogc = s.nogc();
        assert_eq!(elements::get(nogc, a.get(nogc), 1), Value::from_i32(11));
        object::prevent_extensions(s, a).unwrap();
        assert!(!elements::set(s, a, 5, RootedValue::Primitive(Value::NULL)).unwrap());
        assert!(elements::set(s, a, 2, RootedValue::Primitive(Value::NULL)).unwrap());
        assert_eq!(elements::get(s.nogc(), a.get(s.nogc()), 2), Value::NULL);
    });
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MVal {
    Int(i32),
    Obj(usize),
    Str(usize),
}

struct MObj {
    proto: Option<usize>,
    array: bool,
    length: u32,
    elems: BTreeMap<u32, MVal>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_run(seed: u64, steps: usize, stress: bool) {
    let mut vm = vm(stress);
    let mut rng = Rng(seed);
    vm.with(|s, t| {
        let strs: Vec<_> = (0..3)
            .map(|i| {
                let st = string::alloc_str(s, &format!("e{i}"))
                    .unwrap()
                    .bind(s.nogc());
                s.root(st)
            })
            .collect();
        let mut objs: Vec<Root<'_, Object>> = Vec::new();
        let mut model: Vec<MObj> = Vec::new();
        for _ in 0..steps {
            let pick_index = |rng: &mut Rng| match rng.below(10) {
                0 => rng.below(1 << 31) as u32,
                1 => 1000 + rng.below(100) as u32,
                _ => rng.below(40) as u32,
            };
            match rng.below(12) {
                0 if objs.len() < 10 => {
                    let proto = if objs.is_empty() || rng.below(3) == 0 {
                        None
                    } else {
                        Some(rng.below(objs.len() as u64) as usize)
                    };
                    let array = rng.below(2) == 0;
                    let o = if array {
                        elements::new_array(s, t, proto.map(|p| objs[p]))
                    } else {
                        object::new_object(s, t, proto.map(|p| objs[p]), DEFAULT_INLINE)
                    }
                    .unwrap()
                    .bind(s.nogc());
                    objs.push(s.root(o));
                    model.push(MObj {
                        proto,
                        array,
                        length: 0,
                        elems: BTreeMap::new(),
                    });
                }
                _ if objs.is_empty() => {}
                1..=4 => {
                    let o = rng.below(objs.len() as u64) as usize;
                    let i = pick_index(&mut rng);
                    let v = match rng.below(3) {
                        0 => MVal::Int(rng.below(100) as i32),
                        1 => MVal::Obj(rng.below(objs.len() as u64) as usize),
                        _ => MVal::Str(rng.below(3) as usize),
                    };
                    let rv = match v {
                        MVal::Int(n) => RootedValue::Primitive(Value::from_i32(n)),
                        MVal::Obj(n) => RootedValue::Object(objs[n]),
                        MVal::Str(n) => RootedValue::String(strs[n]),
                    };
                    assert!(elements::set(s, objs[o], i, rv).unwrap());
                    let m = &mut model[o];
                    m.elems.insert(i, v);
                    if m.array && i >= m.length {
                        m.length = i + 1;
                    }
                }
                5 => {
                    let o = rng.below(objs.len() as u64) as usize;
                    let i = pick_index(&mut rng);
                    let nogc = s.nogc();
                    assert!(elements::delete(nogc, objs[o].get(nogc), i));
                    model[o].elems.remove(&i);
                }
                6 => {
                    let o = rng.below(objs.len() as u64) as usize;
                    let len = rng.below(50) as u32;
                    let ok = elements::set_length(s, objs[o], len).unwrap();
                    let m = &mut model[o];
                    assert_eq!(ok, m.array);
                    if m.array {
                        m.elems.retain(|&i, _| i < len);
                        m.length = len;
                    }
                }
                7..=9 => {
                    let o = rng.below(objs.len() as u64) as usize;
                    let i = pick_index(&mut rng);
                    let nogc = s.nogc();
                    let got = elements::get(nogc, objs[o].get(nogc), i);
                    let mut cur = Some(o);
                    let mut want = Value::UNDEFINED;
                    while let Some(c) = cur {
                        if let Some(v) = model[c].elems.get(&i) {
                            want = match *v {
                                MVal::Int(n) => Value::from_i32(n),
                                MVal::Obj(n) => Value::object(HeapRef(objs[n].get(nogc).offset())),
                                MVal::Str(n) => Value::string(HeapRef(strs[n].get(nogc).offset())),
                            };
                            break;
                        }
                        cur = model[c].proto;
                    }
                    assert_eq!(got, want, "object {o} index {i}");
                }
                _ => {
                    s.collect();
                }
            }
        }
        let nogc = s.nogc();
        for (o, m) in model.iter().enumerate() {
            let obj = objs[o].get(nogc);
            let want: Vec<u32> = m.elems.keys().copied().collect();
            assert_eq!(elements::keys(nogc, obj), want, "keys of {o}");
            assert_eq!(
                elements::length(nogc, obj),
                if m.array { m.length } else { 0 }
            );
        }
    });
}

#[test]
fn random_operations_match_the_model() {
    for seed in 1..=30 {
        random_run(seed * 0x9e37_79b9, 800, false);
    }
}

#[test]
fn random_operations_match_the_model_under_gc_stress() {
    for seed in 1..=3 {
        random_run(seed * 0x51ed_270b, 300, true);
    }
}

/// Elements reachable only from the store survive collections (the tracer
/// reports dense and sparse values).
#[test]
fn values_held_only_by_elements_survive() {
    for far in [false, true] {
        let mut vm = vm(false);
        vm.with(|s, t| {
            let a = elements::new_array(s, t, None).unwrap().bind(s.nogc());
            let a = s.root(a);
            if far {
                elements::set(s, a, 1 << 30, RootedValue::Primitive(Value::NULL)).unwrap();
            }
            for i in 0..12u32 {
                s.scope(|inner| {
                    let c = object::new_object(inner, t, None, 1)
                        .unwrap()
                        .bind(inner.nogc());
                    let c = inner.root(c);
                    elements::set(inner, a, i, RootedValue::Object(c)).unwrap();
                });
                s.collect();
            }
            // Freed cells get the collector's freed kind byte.
            let nogc = s.nogc();
            for i in 0..12 {
                let v = elements::get(nogc, a.get(nogc), i);
                let ferrojs_value::Kind::Object(r) = v.kind() else {
                    panic!("element {i} is not an object");
                };
                assert_eq!(
                    nogc.cage().read_u8(r.0),
                    ferrojs_vm::CellKind::Object as u8,
                    "sparse {far}: element {i} was freed"
                );
            }
        });
    }
}

#[test]
fn growing_the_length_adds_no_elements() {
    let mut vm = vm(false);
    vm.with(|s, t| {
        let a = elements::new_array(s, t, None).unwrap().bind(s.nogc());
        let a = s.root(a);
        elements::set(s, a, 0, RootedValue::Primitive(Value::TRUE)).unwrap();
        assert!(elements::set_length(s, a, 100).unwrap());
        let nogc = s.nogc();
        assert_eq!(elements::length(nogc, a.get(nogc)), 100);
        assert_eq!(elements::keys(nogc, a.get(nogc)), vec![0]);
        assert_eq!(elements::get(nogc, a.get(nogc), 50), Value::UNDEFINED);
        // Non-extensible and sparse: updates work, new indices don't.
        elements::set(s, a, 1 << 25, RootedValue::Primitive(Value::NULL)).unwrap();
        object::prevent_extensions(s, a).unwrap();
        assert!(elements::set(s, a, 1 << 25, RootedValue::Primitive(Value::FALSE)).unwrap());
        assert!(!elements::set(s, a, 7, RootedValue::Primitive(Value::FALSE)).unwrap());
        let nogc = s.nogc();
        assert!(elements::delete(nogc, a.get(nogc), 0));
        assert_eq!(elements::keys(nogc, a.get(nogc)), vec![1 << 25]);
    });
}
