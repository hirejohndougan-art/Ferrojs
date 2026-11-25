//! Accessor properties and \[\[DefineOwnProperty\]\] with partial
//! descriptors (task 1.8d, D-055), checked by hand and against a plain
//! Rust model of ValidateAndApplyPropertyDescriptor (ECMA-262 §10.1.6.3)
//! through random operations, with the heap verifier on and collections
//! at random points (or before every allocation).

use ferrojs_core::gc::{CxOptions, Root, Scope};
use ferrojs_core::heap::HeapOptions;
use ferrojs_core::NoGc;
use ferrojs_value::{HeapRef, Value};
use ferrojs_vm::object::{self, DEFAULT_INLINE};
use ferrojs_vm::shape::Attrs;
use ferrojs_vm::{
    string, Descriptor, Got, JsString, Object, PropertyValue, RootedValue, SetResult, Tables, Vm,
    VmOptions,
};

fn vm(stress: bool) -> Vm {
    Vm::new(VmOptions {
        heap_size: 64 * ferrojs_core::cage::CHUNK_SIZE as u64,
        heap: HeapOptions {
            poison: true,
            verify: true,
        },
        cx: CxOptions { gc_stress: stress },
        ..VmOptions::default()
    })
    .unwrap()
}

fn atom<'s>(s: &mut Scope<'s>, t: &mut Tables, text: &str) -> Root<'s, JsString> {
    let a = string::atom(s, t, text).unwrap().bind(s.nogc());
    s.root(a)
}

fn obj<'s>(s: &mut Scope<'s>, t: &mut Tables, proto: Option<Root<'_, Object>>) -> Root<'s, Object> {
    let o = object::new_object(s, t, proto, DEFAULT_INLINE)
        .unwrap()
        .bind(s.nogc());
    s.root(o)
}

fn val(nogc: NoGc<'_>, o: Root<'_, Object>) -> Value {
    Value::object(HeapRef(o.get(nogc).offset()))
}

const W: u8 = Attrs::WRITABLE.0;
const E: u8 = Attrs::ENUMERABLE.0;
const C: u8 = Attrs::CONFIGURABLE.0;
const A: u8 = Attrs::ACCESSOR.0;
fn undef<'s>() -> RootedValue<'s> {
    RootedValue::Primitive(Value::UNDEFINED)
}

#[test]
fn getters_and_setters_are_handed_back() {
    let mut vm = vm(true);
    vm.with(|s, t| {
        let o = obj(s, t, None);
        let (g, st) = (obj(s, t, None), obj(s, t, None));
        let k = atom(s, t, "x");
        let desc = Descriptor::accessor(
            RootedValue::Object(g),
            RootedValue::Object(st),
            Attrs(E | C),
        );
        assert!(object::define_property(s, o, k, desc).unwrap());
        s.collect();
        let nogc = s.nogc();
        let (gv, sv) = (val(nogc, g), val(nogc, st));
        assert_eq!(
            object::get_own(nogc, o.get(nogc), k.get(nogc)),
            Some((
                PropertyValue::Accessor { get: gv, set: sv },
                Attrs(A | E | C)
            ))
        );
        assert_eq!(object::get(nogc, o.get(nogc), k.get(nogc)), Got::Getter(gv));
        let one = RootedValue::Primitive(Value::from_i32(1));
        assert_eq!(object::set(s, o, k, one).unwrap(), SetResult::Setter(sv));

        // Inherited: the child's get and set reach the prototype's pair,
        // and the set doesn't add an own property.
        let child = obj(s, t, Some(o));
        let nogc = s.nogc();
        assert_eq!(
            object::get(nogc, child.get(nogc), k.get(nogc)),
            Got::Getter(gv)
        );
        assert_eq!(
            object::set(s, child, k, one).unwrap(),
            SetResult::Setter(sv)
        );
        let nogc = s.nogc();
        assert!(object::get_own(nogc, child.get(nogc), k.get(nogc)).is_none());

        // Without a getter, a read is undefined; without a setter, an
        // assignment is refused (own or inherited).
        let half = Descriptor {
            get: Some(undef()),
            ..Descriptor::default()
        };
        assert!(object::define_property(s, o, k, half).unwrap());
        let nogc = s.nogc();
        assert_eq!(
            object::get(nogc, child.get(nogc), k.get(nogc)),
            Got::Value(Value::UNDEFINED)
        );
        let no_set = Descriptor {
            set: Some(undef()),
            ..Descriptor::default()
        };
        assert!(object::define_property(s, o, k, no_set).unwrap());
        assert_eq!(object::set(s, o, k, one).unwrap(), SetResult::Done(false));
        assert_eq!(
            object::set(s, child, k, one).unwrap(),
            SetResult::Done(false)
        );

        // Deleting it frees the pair; the getter object lives on only
        // through its root.
        assert!(object::delete(s, o, k).unwrap());
        s.collect();
        let nogc = s.nogc();
        assert!(object::get_own(nogc, o.get(nogc), k.get(nogc)).is_none());
        assert_eq!(
            object::get(nogc, child.get(nogc), k.get(nogc)),
            Got::Value(Value::UNDEFINED)
        );
    });
}

#[test]
fn pairs_keep_their_functions_alive() {
    let mut vm = vm(false);
    vm.with(|s, t| {
        let o = obj(s, t, None);
        let mut keys = Vec::new();
        for i in 0..40 {
            let k = atom(s, t, &format!("p{i}"));
            // Only the pair refers to these objects once the inner scope
            // ends.
            s.scope(|n| {
                let g = obj(n, t, None);
                let st = obj(n, t, None);
                let sk = atom(n, t, "tag");
                let tag = RootedValue::Primitive(Value::from_i32(i));
                assert!(object::define(n, g, sk, tag, Attrs::DEFAULT).unwrap());
                let desc =
                    Descriptor::accessor(RootedValue::Object(g), RootedValue::Object(st), Attrs(C));
                assert!(object::define_property(n, o, k, desc).unwrap());
                n.collect();
            });
            keys.push(k);
        }
        s.collect();
        let tag = atom(s, t, "tag");
        let nogc = s.nogc();
        for (i, k) in keys.iter().enumerate() {
            let Some((PropertyValue::Accessor { get, .. }, _)) =
                object::get_own(nogc, o.get(nogc), k.get(nogc))
            else {
                panic!("p{i} is an accessor");
            };
            let ferrojs_value::Kind::Object(r) = get.kind() else {
                panic!("getter is an object");
            };
            let g = nogc.handle::<Object>(r.0).expect("live getter");
            assert_eq!(
                object::get(nogc, g, tag.get(nogc)),
                Got::Value(Value::from_i32(i as i32))
            );
        }
    });
}

#[test]
fn getters_survive_without_other_roots() {
    // The getter and setter are reachable only through the pair, which is
    // reachable only through the object's slot.
    let mut vm = vm(true);
    vm.with(|s, t| {
        let o = obj(s, t, None);
        let k = atom(s, t, "x");
        let tag = atom(s, t, "tag");
        s.scope(|n| {
            let g = obj(n, t, None);
            let v = RootedValue::Primitive(Value::from_i32(7));
            assert!(object::define(n, g, tag, v, Attrs::DEFAULT).unwrap());
            let desc = Descriptor::accessor(RootedValue::Object(g), undef(), Attrs(E | C));
            assert!(object::define_property(n, o, k, desc).unwrap());
        });
        // Fill the heap with garbage and collect a few times.
        for _ in 0..50 {
            let _ = obj(s, t, None);
        }
        s.collect();
        s.collect();
        let nogc = s.nogc();
        let Got::Getter(g) = object::get(nogc, o.get(nogc), k.get(nogc)) else {
            panic!("a getter");
        };
        let ferrojs_value::Kind::Object(r) = g.kind() else {
            panic!("an object");
        };
        let g = nogc.handle::<Object>(r.0).expect("live getter");
        assert_eq!(
            object::get(nogc, g, tag.get(nogc)),
            Got::Value(Value::from_i32(7))
        );
    });
}

#[test]
fn validate_and_apply_by_hand() {
    let mut vm = vm(true);
    vm.with(|s, t| {
        let o = obj(s, t, None);
        let k = atom(s, t, "x");
        let g = obj(s, t, None);
        let one = RootedValue::Primitive(Value::from_i32(1));
        let two = RootedValue::Primitive(Value::from_i32(2));
        let get_attrs = |s: &mut Scope<'_>| {
            let nogc = s.nogc();
            object::get_own(nogc, o.get(nogc), k.get(nogc))
        };

        // A new property from a generic descriptor: an undefined,
        // non-writable, non-enumerable, non-configurable data property.
        // Here start configurable so it can change kind.
        let d = Descriptor {
            configurable: Some(true),
            ..Descriptor::default()
        };
        assert!(object::define_property(s, o, k, d).unwrap());
        assert_eq!(
            get_attrs(s),
            Some((PropertyValue::Data(Value::UNDEFINED), Attrs(C)))
        );

        // Data to accessor keeps enumerable and configurable; the missing
        // setter is undefined.
        let d = Descriptor {
            get: Some(RootedValue::Object(g)),
            ..Descriptor::default()
        };
        assert!(object::define_property(s, o, k, d).unwrap());
        let gv = val(s.nogc(), g);
        assert_eq!(
            get_attrs(s),
            Some((
                PropertyValue::Accessor {
                    get: gv,
                    set: Value::UNDEFINED
                },
                Attrs(A | C)
            ))
        );
        assert!(object::is_dictionary(s.nogc(), o.get(s.nogc())));

        // Accessor to data: writable defaults to false, value to undefined
        // unless given.
        let d = Descriptor {
            value: Some(one),
            enumerable: Some(true),
            ..Descriptor::default()
        };
        assert!(object::define_property(s, o, k, d).unwrap());
        assert_eq!(
            get_attrs(s),
            Some((PropertyValue::Data(Value::from_i32(1)), Attrs(E | C)))
        );

        // Lock it: non-configurable, non-writable.
        let d = Descriptor {
            configurable: Some(false),
            ..Descriptor::default()
        };
        assert!(object::define_property(s, o, k, d).unwrap());
        // Same value: allowed. Different value, writable, configurable,
        // enumerability change, accessor: refused.
        let same = Descriptor {
            value: Some(one),
            writable: Some(false),
            ..Descriptor::default()
        };
        assert!(object::define_property(s, o, k, same).unwrap());
        for d in [
            Descriptor {
                value: Some(two),
                ..Descriptor::default()
            },
            Descriptor {
                writable: Some(true),
                ..Descriptor::default()
            },
            Descriptor {
                configurable: Some(true),
                ..Descriptor::default()
            },
            Descriptor {
                enumerable: Some(false),
                ..Descriptor::default()
            },
            Descriptor {
                get: Some(undef()),
                ..Descriptor::default()
            },
        ] {
            assert!(!object::define_property(s, o, k, d).unwrap());
        }
        assert!(object::define_property(s, o, k, Descriptor::default()).unwrap());
        assert_eq!(
            get_attrs(s),
            Some((PropertyValue::Data(Value::from_i32(1)), Attrs(E)))
        );

        // A non-configurable accessor: only SameValue getters and setters.
        let k2 = atom(s, t, "y");
        let d = Descriptor::accessor(RootedValue::Object(g), undef(), Attrs(0));
        assert!(object::define_property(s, o, k2, d).unwrap());
        let d = Descriptor {
            get: Some(RootedValue::Object(g)),
            set: Some(undef()),
            ..Descriptor::default()
        };
        assert!(object::define_property(s, o, k2, d).unwrap());
        for d in [
            Descriptor {
                get: Some(undef()),
                ..Descriptor::default()
            },
            Descriptor {
                set: Some(RootedValue::Object(g)),
                ..Descriptor::default()
            },
            Descriptor {
                value: Some(one),
                ..Descriptor::default()
            },
            Descriptor {
                writable: Some(false),
                ..Descriptor::default()
            },
        ] {
            assert!(!object::define_property(s, o, k2, d).unwrap());
        }
        // Plain `define` (a complete data descriptor) can't replace it.
        assert!(!object::define(s, o, k2, two, Attrs::DEFAULT).unwrap());

        // A non-extensible object takes no new accessor.
        object::prevent_extensions(s, o).unwrap();
        let k3 = atom(s, t, "z");
        let d = Descriptor::accessor(undef(), undef(), Attrs(C));
        assert!(!object::define_property(s, o, k3, d).unwrap());
    });
}

#[test]
#[should_panic(expected = "either data or accessor")]
fn mixed_descriptors_are_a_caller_bug() {
    let mut vm = vm(false);
    vm.with(|s, t| {
        let o = obj(s, t, None);
        let k = atom(s, t, "x");
        let d = Descriptor {
            value: Some(undef()),
            get: Some(undef()),
            ..Descriptor::default()
        };
        let _ = object::define_property(s, o, k, d);
    });
}

// ---- the model ----

/// A value: an integer, or one of the test's objects by index (standing
/// in for functions: the object model doesn't call them).
#[derive(Clone, Copy, PartialEq, Debug)]
enum MVal {
    Undef,
    Int(i32),
    Obj(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MProp {
    Data { v: MVal, w: bool, e: bool, c: bool },
    Acc { g: MVal, s: MVal, e: bool, c: bool },
}

#[derive(Clone, Copy, Default, Debug)]
struct MDesc {
    value: Option<MVal>,
    writable: Option<bool>,
    get: Option<MVal>,
    set: Option<MVal>,
    enumerable: Option<bool>,
    configurable: Option<bool>,
}

impl MDesc {
    fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }
    fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }
}

struct MObj {
    proto: Option<usize>,
    extensible: bool,
    props: Vec<(usize, MProp)>,
}

/// ValidateAndApplyPropertyDescriptor, written from the spec text.
fn model_define(o: &mut MObj, key: usize, d: MDesc) -> bool {
    let Some(i) = o.props.iter().position(|p| p.0 == key) else {
        if !o.extensible {
            return false;
        }
        let e = d.enumerable.unwrap_or(false);
        let c = d.configurable.unwrap_or(false);
        let p = if d.is_accessor() {
            MProp::Acc {
                g: d.get.unwrap_or(MVal::Undef),
                s: d.set.unwrap_or(MVal::Undef),
                e,
                c,
            }
        } else {
            MProp::Data {
                v: d.value.unwrap_or(MVal::Undef),
                w: d.writable.unwrap_or(false),
                e,
                c,
            }
        };
        o.props.push((key, p));
        return true;
    };
    let current = o.props[i].1;
    let (ce, cc) = match current {
        MProp::Data { e, c, .. } | MProp::Acc { e, c, .. } => (e, c),
    };
    let cur_acc = matches!(current, MProp::Acc { .. });
    if !cc {
        if d.configurable == Some(true) {
            return false;
        }
        if d.enumerable.is_some_and(|e| e != ce) {
            return false;
        }
        let generic = !d.is_accessor() && !d.is_data();
        if !generic && d.is_accessor() != cur_acc {
            return false;
        }
        match current {
            MProp::Acc { g, s, .. } => {
                if d.get.is_some_and(|x| x != g) || d.set.is_some_and(|x| x != s) {
                    return false;
                }
            }
            MProp::Data { v, w, .. } => {
                if !w && (d.writable == Some(true) || d.value.is_some_and(|x| x != v)) {
                    return false;
                }
            }
        }
    }
    let e = d.enumerable.unwrap_or(ce);
    let c = d.configurable.unwrap_or(cc);
    let new = match current {
        MProp::Data { .. } if d.is_accessor() => MProp::Acc {
            g: d.get.unwrap_or(MVal::Undef),
            s: d.set.unwrap_or(MVal::Undef),
            e,
            c,
        },
        MProp::Acc { .. } if d.is_data() => MProp::Data {
            v: d.value.unwrap_or(MVal::Undef),
            w: d.writable.unwrap_or(false),
            e,
            c,
        },
        MProp::Acc { g, s, .. } => MProp::Acc {
            g: d.get.unwrap_or(g),
            s: d.set.unwrap_or(s),
            e,
            c,
        },
        MProp::Data { v, w, .. } => MProp::Data {
            v: d.value.unwrap_or(v),
            w: d.writable.unwrap_or(w),
            e,
            c,
        },
    };
    o.props[i].1 = new;
    true
}

#[derive(PartialEq, Debug)]
enum MGot {
    Value(MVal),
    Getter(MVal),
}

fn model_get(m: &[MObj], mut o: usize, key: usize) -> MGot {
    loop {
        if let Some((_, p)) = m[o].props.iter().find(|p| p.0 == key) {
            return match *p {
                MProp::Data { v, .. } => MGot::Value(v),
                MProp::Acc { g: MVal::Undef, .. } => MGot::Value(MVal::Undef),
                MProp::Acc { g, .. } => MGot::Getter(g),
            };
        }
        match m[o].proto {
            Some(p) => o = p,
            None => return MGot::Value(MVal::Undef),
        }
    }
}

#[derive(PartialEq, Debug)]
enum MSet {
    Done(bool),
    Setter(MVal),
}

/// OrdinarySet with the object as receiver.
fn model_set(m: &mut [MObj], o: usize, key: usize, v: MVal) -> MSet {
    let mut cur = Some(o);
    while let Some(c) = cur {
        if let Some(&(_, p)) = m[c].props.iter().find(|p| p.0 == key) {
            match p {
                MProp::Acc { s: MVal::Undef, .. } => return MSet::Done(false),
                MProp::Acc { s, .. } => return MSet::Setter(s),
                MProp::Data { w: false, .. } => return MSet::Done(false),
                MProp::Data { .. } if c == o => {
                    let i = m[o].props.iter().position(|p| p.0 == key).unwrap();
                    if let MProp::Data { v: old, .. } = &mut m[o].props[i].1 {
                        *old = v;
                    }
                    return MSet::Done(true);
                }
                MProp::Data { .. } => break,
            }
        }
        cur = m[c].proto;
    }
    if !m[o].extensible {
        return MSet::Done(false);
    }
    m[o].props.push((
        key,
        MProp::Data {
            v,
            w: true,
            e: true,
            c: true,
        },
    ));
    MSet::Done(true)
}

fn model_delete(o: &mut MObj, key: usize) -> bool {
    match o.props.iter().position(|p| p.0 == key) {
        None => true,
        Some(i) => match o.props[i].1 {
            MProp::Data { c: false, .. } | MProp::Acc { c: false, .. } => false,
            _ => {
                o.props.remove(i);
                true
            }
        },
    }
}

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
    fn maybe<T>(&mut self, f: impl FnOnce(&mut Rng) -> T) -> Option<T> {
        (self.below(2) == 0).then(|| f(self))
    }
}

fn random_run(seed: u64, steps: usize, stress: bool) {
    let mut vm = vm(stress);
    let mut rng = Rng(seed);
    vm.with(|s, t| {
        let keys: Vec<_> = (0..5).map(|i| atom(s, t, &format!("k{i}"))).collect();
        let mut objs: Vec<Root<'_, Object>> = Vec::new();
        let mut model: Vec<MObj> = Vec::new();
        let to_value = |nogc: NoGc<'_>, objs: &[Root<'_, Object>], v: MVal| match v {
            MVal::Undef => Value::UNDEFINED,
            MVal::Int(i) => Value::from_i32(i),
            MVal::Obj(i) => val(nogc, objs[i]),
        };
        for _ in 0..steps {
            let pick_val = |rng: &mut Rng, n: usize| match rng.below(4) {
                0 => MVal::Undef,
                1 | 2 if n > 0 => MVal::Obj(rng.below(n)),
                _ => MVal::Int(rng.below(3) as i32),
            };
            match rng.below(16) {
                0 if objs.len() < 8 => {
                    let proto =
                        (!objs.is_empty() && rng.below(2) == 0).then(|| rng.below(objs.len()));
                    let inline = [0, 1, 4][rng.below(3)];
                    let o = object::new_object(s, t, proto.map(|p| objs[p]), inline)
                        .unwrap()
                        .bind(s.nogc());
                    objs.push(s.root(o));
                    model.push(MObj {
                        proto,
                        extensible: true,
                        props: Vec::new(),
                    });
                }
                _ if objs.is_empty() => {}
                1..=6 => {
                    let (o, k) = (rng.below(objs.len()), rng.below(keys.len()));
                    let n = objs.len();
                    let b = |rng: &mut Rng| rng.maybe(|r| r.below(2) == 0);
                    let mut d = MDesc {
                        enumerable: b(&mut rng),
                        configurable: b(&mut rng),
                        ..MDesc::default()
                    };
                    match rng.below(3) {
                        0 => {
                            d.value = rng.maybe(|r| pick_val(r, n));
                            d.writable = b(&mut rng);
                        }
                        1 => {
                            d.get = rng.maybe(|r| pick_val(r, n));
                            d.set = rng.maybe(|r| pick_val(r, n));
                        }
                        _ => {}
                    }
                    let rv = |v: Option<MVal>| {
                        v.map(|v| match v {
                            MVal::Undef => undef(),
                            MVal::Int(i) => RootedValue::Primitive(Value::from_i32(i)),
                            MVal::Obj(i) => RootedValue::Object(objs[i]),
                        })
                    };
                    let desc = Descriptor {
                        value: rv(d.value),
                        writable: d.writable,
                        get: rv(d.get),
                        set: rv(d.set),
                        enumerable: d.enumerable,
                        configurable: d.configurable,
                    };
                    let ok = object::define_property(s, objs[o], keys[k], desc).unwrap();
                    assert_eq!(ok, model_define(&mut model[o], k, d), "define {d:?}");
                }
                7 | 8 => {
                    let (o, k) = (rng.below(objs.len()), rng.below(keys.len()));
                    let v = pick_val(&mut rng, objs.len());
                    let rv = match v {
                        MVal::Undef => undef(),
                        MVal::Int(i) => RootedValue::Primitive(Value::from_i32(i)),
                        MVal::Obj(i) => RootedValue::Object(objs[i]),
                    };
                    let got = object::set(s, objs[o], keys[k], rv).unwrap();
                    let want = match model_set(&mut model, o, k, v) {
                        MSet::Done(b) => SetResult::Done(b),
                        MSet::Setter(f) => SetResult::Setter(to_value(s.nogc(), &objs, f)),
                    };
                    assert_eq!(got, want, "set");
                }
                9 => {
                    let (o, k) = (rng.below(objs.len()), rng.below(keys.len()));
                    let ok = object::delete(s, objs[o], keys[k]).unwrap();
                    assert_eq!(ok, model_delete(&mut model[o], k), "delete");
                }
                10 if rng.below(6) == 0 => {
                    let o = rng.below(objs.len());
                    object::prevent_extensions(s, objs[o]).unwrap();
                    model[o].extensible = false;
                }
                11..=13 => {
                    let (o, k) = (rng.below(objs.len()), rng.below(keys.len()));
                    let nogc = s.nogc();
                    let got = object::get(nogc, objs[o].get(nogc), keys[k].get(nogc));
                    let want = match model_get(&model, o, k) {
                        MGot::Value(v) => Got::Value(to_value(nogc, &objs, v)),
                        MGot::Getter(g) => Got::Getter(to_value(nogc, &objs, g)),
                    };
                    assert_eq!(got, want, "get object {o} key k{k}");
                }
                _ => {
                    s.collect();
                }
            }
        }
        // Every object has exactly the model's own properties, in order.
        let nogc = s.nogc();
        for (o, m) in model.iter().enumerate() {
            let obj = objs[o].get(nogc);
            let own: Vec<usize> = object::own_keys(nogc, obj)
                .into_iter()
                .map(|k| {
                    keys.iter()
                        .position(|r| r.get(nogc).offset() == k.offset())
                        .unwrap()
                })
                .collect();
            let want: Vec<usize> = m.props.iter().map(|p| p.0).collect();
            assert_eq!(own, want, "keys of object {o}");
            for &(k, p) in &m.props {
                let got = object::get_own(nogc, obj, keys[k].get(nogc)).unwrap();
                let want = match p {
                    MProp::Data { v, w, e, c } => (
                        PropertyValue::Data(to_value(nogc, &objs, v)),
                        Attrs(bit(w, W) | bit(e, E) | bit(c, C)),
                    ),
                    MProp::Acc { g, s: st, e, c } => (
                        PropertyValue::Accessor {
                            get: to_value(nogc, &objs, g),
                            set: to_value(nogc, &objs, st),
                        },
                        Attrs(A | bit(e, E) | bit(c, C)),
                    ),
                };
                assert_eq!(got, want, "object {o} key k{k}");
            }
        }
    });
}

#[test]
fn random_descriptors_match_the_model() {
    for seed in 1..=40u64 {
        random_run(seed * 0x9e37_79b9_7f4a_7c15, 400, false);
    }
}

#[test]
fn random_descriptors_match_the_model_under_gc_stress() {
    for seed in 1..=6u64 {
        random_run(seed * 0x2545_f491_4f6c_dd1d, 250, true);
    }
}

fn bit(on: bool, b: u8) -> u8 {
    if on {
        b
    } else {
        0
    }
}
