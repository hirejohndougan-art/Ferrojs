//! Strings, atoms, shapes and objects (task 1.8a, D-038), checked against
//! a plain Rust model through random operations, with the heap verifier
//! on and collections at random points (or before every allocation).

use std::collections::HashMap;

use ferrojs_core::gc::{CxOptions, Root, Scope};
use ferrojs_core::heap::HeapOptions;
use ferrojs_value::{HeapRef, Kind, Value};
use ferrojs_vm::object::{self, DEFAULT_INLINE};
use ferrojs_vm::shape::{self, Attrs};
use ferrojs_vm::{string, JsString, Object, RootedValue, Tables, Vm, VmOptions};

fn vm(stress: bool) -> Vm {
    Vm::new(VmOptions {
        heap_size: 64 * ferrojs_core::cage::CHUNK_SIZE as u64,
        heap: HeapOptions {
            poison: true,
            verify: true,
        },
        cx: CxOptions { gc_stress: stress },
    })
    .unwrap()
}

fn atom<'s>(s: &mut Scope<'s>, t: &mut Tables, text: &str) -> Root<'s, JsString> {
    let a = string::atom(s, t, text).unwrap().bind(s.nogc());
    s.root(a)
}

#[test]
fn strings_round_trip_and_atoms_are_interned() {
    let mut vm = vm(true);
    vm.with(|s, t| {
        for text in [
            "",
            "a",
            "hello, world",
            "caf\u{e9}",
            "\u{1F600} snow \u{2603}",
        ] {
            let str_ = string::alloc_str(s, text).unwrap().bind(s.nogc());
            assert_eq!(string::to_rust(s.nogc(), str_), text);
            assert_eq!(
                string::len(s.nogc(), str_) as usize,
                text.encode_utf16().count()
            );
            assert!(!string::is_atom(s.nogc(), str_));
        }
        // Lone surrogates are representable.
        let lone = string::alloc_units(s, &[0xd800, 0x41])
            .unwrap()
            .bind(s.nogc());
        assert_eq!(string::units(s.nogc(), lone), vec![0xd800, 0x41]);

        let a = atom(s, t, "key");
        let n = t.atom_count();
        s.collect();
        let b = atom(s, t, "key");
        assert_eq!(a.get(s.nogc()).offset(), b.get(s.nogc()).offset());
        assert_eq!(t.atom_count(), n);
        assert!(string::is_atom(s.nogc(), a.get(s.nogc())));
    });
}

#[test]
fn objects_built_the_same_way_share_shapes() {
    let mut vm = vm(false);
    vm.with(|s, t| {
        let (x, y, z) = (atom(s, t, "x"), atom(s, t, "y"), atom(s, t, "z"));
        let mut objs = Vec::new();
        for i in 0..3 {
            let o = object::new_object(s, t, None, DEFAULT_INLINE)
                .unwrap()
                .bind(s.nogc());
            let o = s.root(o);
            let v = RootedValue::Primitive(Value::from_i32(i));
            assert!(object::set(s, o, x, v).unwrap());
            assert!(object::set(s, o, y, v).unwrap());
            objs.push(o);
        }
        let shape_of =
            |s: &Scope<'_>, o: Root<'_, Object>| object::shape(s.nogc(), o.get(s.nogc())).offset();
        assert_eq!(shape_of(s, objs[0]), shape_of(s, objs[1]));
        assert_eq!(shape_of(s, objs[1]), shape_of(s, objs[2]));
        // A different order is a different shape; so is another property.
        let o = object::new_object(s, t, None, DEFAULT_INLINE)
            .unwrap()
            .bind(s.nogc());
        let o = s.root(o);
        let v = RootedValue::Primitive(Value::NULL);
        object::set(s, o, y, v).unwrap();
        object::set(s, o, x, v).unwrap();
        assert_ne!(shape_of(s, o), shape_of(s, objs[0]));
        object::set(s, objs[2], z, v).unwrap();
        assert_ne!(shape_of(s, objs[2]), shape_of(s, objs[0]));
        // Insertion order is kept.
        let keys: Vec<String> = object::own_keys(s.nogc(), objs[2].get(s.nogc()))
            .into_iter()
            .map(|k| string::to_rust(s.nogc(), k))
            .collect();
        assert_eq!(keys, ["x", "y", "z"]);
        // Updating a property keeps the shape.
        let before = shape_of(s, objs[0]);
        object::set(s, objs[0], x, RootedValue::Primitive(Value::from_i32(9))).unwrap();
        assert_eq!(shape_of(s, objs[0]), before);
    });
}

#[test]
fn many_properties_spill_into_out_of_line_slots() {
    for inline in [0, 1, DEFAULT_INLINE, 9] {
        let mut vm = vm(false);
        vm.with(|s, t| {
            let o = object::new_object(s, t, None, inline)
                .unwrap()
                .bind(s.nogc());
            let o = s.root(o);
            let keys: Vec<_> = (0..60).map(|i| atom(s, t, &format!("p{i}"))).collect();
            for (i, &k) in keys.iter().enumerate() {
                let v = RootedValue::Primitive(Value::from_i32(i as i32 * 3));
                assert!(object::set(s, o, k, v).unwrap());
                if i % 7 == 0 {
                    s.collect();
                }
            }
            for (i, &k) in keys.iter().enumerate() {
                let v = object::get(s.nogc(), o.get(s.nogc()), k.get(s.nogc()));
                assert_eq!(v, Value::from_i32(i as i32 * 3), "inline {inline}, p{i}");
            }
            assert_eq!(
                shape::count(s.nogc(), object::shape(s.nogc(), o.get(s.nogc()))),
                60
            );
        });
    }
}

#[test]
fn non_writable_properties_refuse_assignment_own_and_inherited() {
    let mut vm = vm(true);
    vm.with(|s, t| {
        let k = atom(s, t, "k");
        let proto = object::new_object(s, t, None, DEFAULT_INLINE)
            .unwrap()
            .bind(s.nogc());
        let proto = s.root(proto);
        let ro = Attrs(Attrs::ENUMERABLE.0 | Attrs::CONFIGURABLE.0);
        let one = RootedValue::Primitive(Value::from_i32(1));
        let two = RootedValue::Primitive(Value::from_i32(2));
        assert!(object::define(s, proto, k, one, ro).unwrap());
        assert!(!object::set(s, proto, k, two).unwrap());
        assert_eq!(
            object::get(s.nogc(), proto.get(s.nogc()), k.get(s.nogc())),
            Value::from_i32(1)
        );
        let child = object::new_object(s, t, Some(proto), DEFAULT_INLINE)
            .unwrap()
            .bind(s.nogc());
        let child = s.root(child);
        assert_eq!(
            object::get(s.nogc(), child.get(s.nogc()), k.get(s.nogc())),
            Value::from_i32(1)
        );
        assert!(
            !object::set(s, child, k, two).unwrap(),
            "inherited read-only"
        );
        assert!(object::get_own(s.nogc(), child.get(s.nogc()), k.get(s.nogc())).is_none());
        // Redefining with other attributes needs dictionary mode (1.8b).
        assert!(!object::define(s, proto, k, two, Attrs::DEFAULT).unwrap());
        assert!(object::define(s, proto, k, two, ro).unwrap());
        assert_eq!(
            object::get(s.nogc(), child.get(s.nogc()), k.get(s.nogc())),
            Value::from_i32(2)
        );
    });
}

/// The model: per object, its prototype and own properties.
#[derive(Clone, Copy, PartialEq, Debug)]
enum MVal {
    Int(i32),
    Obj(usize),
    Str(usize),
}

struct MObj {
    proto: Option<usize>,
    props: HashMap<usize, (MVal, bool)>,
}

fn model_get(objs: &[MObj], mut o: usize, k: usize) -> Option<MVal> {
    loop {
        if let Some(&(v, _)) = objs[o].props.get(&k) {
            return Some(v);
        }
        o = objs[o].proto?;
    }
}

fn model_set(objs: &mut [MObj], o: usize, k: usize, v: MVal) -> bool {
    if let Some(p) = objs[o].props.get_mut(&k) {
        if !p.1 {
            return false;
        }
        p.0 = v;
        return true;
    }
    let mut cur = objs[o].proto;
    while let Some(p) = cur {
        if let Some(&(_, writable)) = objs[p].props.get(&k) {
            if !writable {
                return false;
            }
            break;
        }
        cur = objs[p].proto;
    }
    objs[o].props.insert(k, (v, true));
    true
}

/// A small deterministic generator (no dependency needed).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn random_model_run(seed: u64, steps: usize, stress: bool) {
    let mut vm = vm(stress);
    let mut rng = Rng(seed);
    vm.with(|s, t| {
        let keys: Vec<_> = (0..6).map(|i| atom(s, t, &format!("k{i}"))).collect();
        let strs: Vec<_> = (0..3)
            .map(|i| {
                let st = string::alloc_str(s, &format!("value {i}"))
                    .unwrap()
                    .bind(s.nogc());
                s.root(st)
            })
            .collect();
        let mut objs: Vec<Root<'_, Object>> = Vec::new();
        let mut model: Vec<MObj> = Vec::new();
        for _ in 0..steps {
            match rng.below(10) {
                0 | 1 if objs.len() < 12 => {
                    let proto = if objs.is_empty() || rng.below(3) == 0 {
                        None
                    } else {
                        Some(rng.below(objs.len()))
                    };
                    let inline = [0, 1, 4][rng.below(3)];
                    let o = object::new_object(s, t, proto.map(|p| objs[p]), inline)
                        .unwrap()
                        .bind(s.nogc());
                    objs.push(s.root(o));
                    model.push(MObj {
                        proto,
                        props: HashMap::new(),
                    });
                }
                _ if objs.is_empty() => {}
                2..=5 => {
                    let (o, k) = (rng.below(objs.len()), rng.below(keys.len()));
                    let v = match rng.below(3) {
                        0 => MVal::Int(rng.below(1000) as i32),
                        1 => MVal::Obj(rng.below(objs.len())),
                        _ => MVal::Str(rng.below(strs.len())),
                    };
                    let rv = match v {
                        MVal::Int(i) => RootedValue::Primitive(Value::from_i32(i)),
                        MVal::Obj(i) => RootedValue::Object(objs[i]),
                        MVal::Str(i) => RootedValue::String(strs[i]),
                    };
                    let readonly = rng.below(8) == 0;
                    if readonly {
                        let attrs = Attrs(Attrs::ENUMERABLE.0);
                        let fresh = !model[o].props.contains_key(&k);
                        let ok = object::define(s, objs[o], keys[k], rv, attrs).unwrap();
                        // A frozen (non-writable, non-configurable) property
                        // only accepts its own value again.
                        let expect = fresh || (!model[o].props[&k].1 && model[o].props[&k].0 == v);
                        assert_eq!(ok, expect);
                        if ok {
                            model[o].props.insert(k, (v, false));
                        }
                    } else {
                        let ok = object::set(s, objs[o], keys[k], rv).unwrap();
                        assert_eq!(ok, model_set(&mut model, o, k, v));
                    }
                }
                6..=8 => {
                    let (o, k) = (rng.below(objs.len()), rng.below(keys.len()));
                    let nogc = s.nogc();
                    let got = object::get(nogc, objs[o].get(nogc), keys[k].get(nogc));
                    let want = match model_get(&model, o, k) {
                        None => Value::UNDEFINED,
                        Some(MVal::Int(i)) => Value::from_i32(i),
                        Some(MVal::Obj(i)) => Value::object(HeapRef(objs[i].get(nogc).offset())),
                        Some(MVal::Str(i)) => Value::string(HeapRef(strs[i].get(nogc).offset())),
                    };
                    assert_eq!(got, want, "object {o} key k{k}");
                    if let Kind::Object(r) = got.kind() {
                        assert!(objs.iter().any(|o| o.get(nogc).offset() == r.0));
                    }
                }
                _ => {
                    s.collect();
                }
            }
        }
        // Every object still reads back the model exactly.
        let nogc = s.nogc();
        for (o, m) in model.iter().enumerate() {
            let own: Vec<usize> = object::own_keys(nogc, objs[o].get(nogc))
                .into_iter()
                .map(|k| {
                    keys.iter()
                        .position(|r| r.get(nogc).offset() == k.offset())
                        .unwrap()
                })
                .collect();
            assert_eq!(own.len(), m.props.len());
        }
    });
}

#[test]
fn random_operations_match_the_model() {
    for seed in 1..=40 {
        random_model_run(seed * 0x9e37_79b9, 400, false);
    }
}

#[test]
fn random_operations_match_the_model_under_gc_stress() {
    for seed in 1..=4 {
        random_model_run(seed * 0x51ed_270b, 150, true);
    }
}

#[test]
fn frozen_properties_only_accept_their_own_value() {
    let mut vm = vm(false);
    vm.with(|s, t| {
        let k = atom(s, t, "k");
        let o = object::new_object(s, t, None, DEFAULT_INLINE)
            .unwrap()
            .bind(s.nogc());
        let o = s.root(o);
        let frozen = Attrs(Attrs::ENUMERABLE.0);
        let v = |i| RootedValue::Primitive(Value::from_i32(i));
        assert!(object::define(s, o, k, v(1), frozen).unwrap());
        assert!(!object::define(s, o, k, v(2), frozen).unwrap());
        assert!(object::define(s, o, k, v(1), frozen).unwrap());
        // SameValue: NaN is NaN, +0 isn't -0, strings compare by text.
        let n = atom(s, t, "n");
        let nan = RootedValue::Primitive(Value::from_f64(f64::NAN));
        assert!(object::define(s, o, n, nan, frozen).unwrap());
        assert!(object::define(s, o, n, nan, frozen).unwrap());
        let z = atom(s, t, "z");
        let pz = RootedValue::Primitive(Value::from_i32(0));
        assert!(object::define(s, o, z, pz, frozen).unwrap());
        let nz = RootedValue::Primitive(Value::from_f64(-0.0));
        assert!(!object::define(s, o, z, nz, frozen).unwrap());
        let st = atom(s, t, "s");
        let a = string::alloc_str(s, "same").unwrap().bind(s.nogc());
        let a = s.root(a);
        let b = string::alloc_str(s, "same").unwrap().bind(s.nogc());
        let b = s.root(b);
        let c = string::alloc_str(s, "other").unwrap().bind(s.nogc());
        let c = s.root(c);
        assert!(object::define(s, o, st, RootedValue::String(a), frozen).unwrap());
        assert!(object::define(s, o, st, RootedValue::String(b), frozen).unwrap());
        assert!(!object::define(s, o, st, RootedValue::String(c), frozen).unwrap());
        // Writable or configurable: the value may change.
        let w = atom(s, t, "w");
        let configurable = Attrs(Attrs::CONFIGURABLE.0);
        assert!(object::define(s, o, w, v(1), configurable).unwrap());
        assert!(object::define(s, o, w, v(2), configurable).unwrap());
        assert_eq!(
            object::get(s.nogc(), o.get(s.nogc()), w.get(s.nogc())),
            Value::from_i32(2)
        );
    });
}

/// The tracer reports every reference an object holds: its shape, its
/// out-of-line slots, and each reference in an inline or out-of-line slot;
/// a shape reports its prototype, parent and key.
#[test]
fn the_tracer_reports_every_reference() {
    use ferrojs_core::Trace;
    let mut vm = vm(false);
    vm.with(|s, t| {
        let proto = object::new_object(s, t, None, 1).unwrap().bind(s.nogc());
        let proto = s.root(proto);
        let o = object::new_object(s, t, Some(proto), 1)
            .unwrap()
            .bind(s.nogc());
        let o = s.root(o);
        let mut expected = Vec::new();
        for i in 0..7 {
            let k = atom(s, t, &format!("k{i}"));
            let v = if i % 2 == 0 {
                let child = object::new_object(s, t, None, 0).unwrap().bind(s.nogc());
                let child = s.root(child);
                expected.push(child.get(s.nogc()).offset());
                RootedValue::Object(child)
            } else {
                let st = string::alloc_str(s, &format!("v{i}"))
                    .unwrap()
                    .bind(s.nogc());
                let st = s.root(st);
                expected.push(st.get(s.nogc()).offset());
                RootedValue::String(st)
            };
            object::set(s, o, k, v).unwrap();
        }
        let nogc = s.nogc();
        let obj = o.get(nogc);
        let mut edges = Vec::new();
        ferrojs_vm::Tracer.trace(nogc.cage(), obj.offset(), nogc.cell_size(obj), &mut edges);
        let shape = object::shape(nogc, obj);
        assert!(edges.contains(&shape.offset()), "shape");
        // One value is inline; the rest are in the out-of-line slots cell,
        // which the object reports and which reports them in turn.
        let slots = nogc.cage().read_u32(obj.offset() + 12);
        assert_ne!(slots, 0);
        assert!(edges.contains(&slots), "slots cell");
        let slots_cell = 8 + 8 * (nogc.cage().read_u64(slots) >> 32) as u32;
        ferrojs_vm::Tracer.trace(nogc.cage(), slots, slots_cell, &mut edges);
        for e in &expected {
            assert!(edges.contains(e), "value {e:#x} in a slot");
        }
        let mut shape_edges = Vec::new();
        ferrojs_vm::Tracer.trace(
            nogc.cage(),
            shape.offset(),
            nogc.cell_size(shape),
            &mut shape_edges,
        );
        assert!(shape_edges.contains(&proto.get(nogc).offset()), "prototype");
        let last_key = object::own_keys(nogc, obj).pop().unwrap();
        assert!(shape_edges.contains(&last_key.offset()), "key");
    });
}

/// Cells reachable only through slots and shapes survive collections.
#[test]
fn cells_reachable_only_through_edges_survive() {
    for stress in [false, true] {
        let mut vm = vm(stress);
        vm.with(|s, t| {
            let o = object::new_object(s, t, None, 1).unwrap().bind(s.nogc());
            let o = s.root(o);
            let keys: Vec<_> = (0..6).map(|i| atom(s, t, &format!("k{i}"))).collect();
            // Objects held only by o's slots (inline and out-of-line, across
            // a growth of the slots cell), each with a prototype held only by
            // its shape.
            for (i, &k) in keys.iter().enumerate() {
                s.scope(|inner| {
                    let p = object::new_object(inner, t, None, 1)
                        .unwrap()
                        .bind(inner.nogc());
                    let p = inner.root(p);
                    let mark = atom(inner, t, "mark");
                    let v = RootedValue::Primitive(Value::from_i32(100 + i as i32));
                    object::set(inner, p, mark, v).unwrap();
                    let c = object::new_object(inner, t, Some(p), 0)
                        .unwrap()
                        .bind(inner.nogc());
                    let c = inner.root(c);
                    object::set(inner, o, k, RootedValue::Object(c)).unwrap();
                });
                s.collect();
            }
            let bytes = s.allocated_bytes();
            s.collect();
            assert_eq!(s.allocated_bytes(), bytes, "everything left is reachable");
            // Each child's prototype still reads back through its shape.
            let mark = atom(s, t, "mark");
            let nogc = s.nogc();
            let obj = o.get(nogc);
            for (i, &k) in keys.iter().enumerate() {
                let v = object::get(nogc, obj, k.get(nogc));
                let Kind::Object(r) = v.kind() else {
                    panic!("k{i} lost its object")
                };
                // Find the child among the objects whose prototype carries the
                // mark: read the child's shape through the cage.
                let shape_offset = (nogc.cage().read_u64(r.0) >> 32) as u32;
                let proto_offset = nogc.cage().read_u32(shape_offset + 12);
                assert_ne!(proto_offset, 0, "k{i}: prototype kept");
                let _ = mark;
            }
        });
    }
}
