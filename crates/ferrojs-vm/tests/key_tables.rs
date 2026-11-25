//! Hashed key tables for large shared shapes and dictionary shapes (task
//! 1.8f, D-060): lookups agree with a plain walk of the keys, tables are
//! shared along a chain and copied at branches and growth, and the tracer
//! keeps keys reachable only through a table alive.

use ferrojs_core::gc::{CxOptions, Root, Scope};
use ferrojs_core::heap::HeapOptions;
use ferrojs_core::Trace;
use ferrojs_value::Value;
use ferrojs_vm::object::{self, DEFAULT_INLINE};
use ferrojs_vm::shape::{self, Attrs, TABLE_MIN};
use ferrojs_vm::{string, Got, JsString, Object, RootedValue, SetResult, Tables, Vm, VmOptions};

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

fn new_obj<'s>(s: &mut Scope<'s>, t: &mut Tables) -> Root<'s, Object> {
    let o = object::new_object(s, t, None, DEFAULT_INLINE)
        .unwrap()
        .bind(s.nogc());
    s.root(o)
}

fn set_int(s: &mut Scope<'_>, o: Root<'_, Object>, k: Root<'_, JsString>, i: i32) {
    let v = RootedValue::Primitive(Value::from_i32(i));
    assert!(object::set(s, o, k, v).map(SetResult::unwrap_done).unwrap());
}

fn get(s: &mut Scope<'_>, o: Root<'_, Object>, k: Root<'_, JsString>) -> Value {
    let nogc = s.nogc();
    match object::get(nogc, o.get(nogc), k.get(nogc)) {
        Got::Value(v) => v,
        Got::Getter(_) => panic!("unexpected getter"),
    }
}

/// The key table field of a shape (offset 32), read through the cage.
fn table_of(s: &mut Scope<'_>, o: Root<'_, Object>) -> u32 {
    let nogc = s.nogc();
    let sh = object::shape(nogc, o.get(nogc));
    nogc.cage().read_u32(sh.offset() + 32)
}

/// Every shape along a long chain finds exactly its own keys, at the slot
/// it was added in, across table growth (16, 32, 64, 128 entries).
#[test]
fn long_chains_find_every_key_at_its_slot() {
    for stress in [false, true] {
        let mut vm = vm(stress);
        vm.with(|s, t| {
            let n = object::MAX_SHARED_PROPERTIES;
            let keys: Vec<_> = (0..n + 5).map(|i| atom(s, t, &format!("c{i}"))).collect();
            let o = new_obj(s, t);
            for i in 0..n as usize {
                set_int(s, o, keys[i], i as i32);
                let has_table = table_of(s, o) != 0;
                assert_eq!(has_table, i as u32 + 1 >= TABLE_MIN, "after {i}");
                // The shape answers for every key, present or not.
                let nogc = s.nogc();
                let sh = object::shape(nogc, o.get(nogc));
                for (j, k) in keys.iter().enumerate() {
                    let p = shape::lookup(nogc, sh, k.get(nogc));
                    if j <= i {
                        let p = p.unwrap_or_else(|| panic!("c{j} missing after {i}"));
                        assert_eq!(p.slot, j as u32);
                        assert_eq!(p.attrs, Attrs::DEFAULT);
                    } else {
                        assert_eq!(p, None, "c{j} present after {i}");
                    }
                }
            }
            assert!(!object::is_dictionary(s.nogc(), o.get(s.nogc())));
            s.collect();
            for (i, &k) in keys.iter().enumerate() {
                let want = if (i as u32) < n {
                    Value::from_i32(i as i32)
                } else {
                    Value::UNDEFINED
                };
                assert_eq!(get(s, o, k), want, "c{i}");
            }
            let nogc = s.nogc();
            let order: Vec<u32> = object::own_keys(nogc, o.get(nogc))
                .iter()
                .map(|k| k.offset())
                .collect();
            let want: Vec<u32> = keys[..n as usize]
                .iter()
                .map(|k| k.get(nogc).offset())
                .collect();
            assert_eq!(order, want);
        });
    }
}

/// Objects built the same way share one table; a branch after the table
/// starts copies it, and neither branch sees the other's keys (the shared
/// table has entries past a shape's count).
#[test]
fn branches_share_a_prefix_and_never_see_each_others_keys() {
    for stress in [false, true] {
        let mut vm = vm(stress);
        vm.with(|s, t| {
            let common: Vec<_> = (0..10).map(|i| atom(s, t, &format!("p{i}"))).collect();
            let left: Vec<_> = (0..6).map(|i| atom(s, t, &format!("l{i}"))).collect();
            let right: Vec<_> = (0..6).map(|i| atom(s, t, &format!("r{i}"))).collect();
            let a = new_obj(s, t);
            let b = new_obj(s, t);
            let c = new_obj(s, t);
            for (i, &k) in common.iter().enumerate() {
                set_int(s, a, k, i as i32);
                set_int(s, b, k, 100 + i as i32);
            }
            assert_eq!(table_of(s, a), table_of(s, b), "one table for one chain");
            // `a` extends the chain in place; `b` branches and copies.
            for (i, &k) in left.iter().enumerate() {
                set_int(s, a, k, 20 + i as i32);
            }
            for (i, &k) in right.iter().enumerate() {
                set_int(s, b, k, 30 + i as i32);
            }
            assert_ne!(table_of(s, a), table_of(s, b));
            // `c` stops inside the shared prefix: the table has entries past
            // its shape's count.
            for &k in &common[..9] {
                set_int(s, c, k, 7);
            }
            s.collect();
            for (i, &k) in common.iter().enumerate() {
                assert_eq!(get(s, a, k), Value::from_i32(i as i32));
                assert_eq!(get(s, b, k), Value::from_i32(100 + i as i32));
                let want = if i < 9 {
                    Value::from_i32(7)
                } else {
                    Value::UNDEFINED
                };
                assert_eq!(get(s, c, k), want, "c: p{i}");
            }
            for (i, (&l, &r)) in left.iter().zip(&right).enumerate() {
                assert_eq!(get(s, a, l), Value::from_i32(20 + i as i32));
                assert_eq!(get(s, a, r), Value::UNDEFINED);
                assert_eq!(get(s, b, r), Value::from_i32(30 + i as i32));
                assert_eq!(get(s, b, l), Value::UNDEFINED);
                assert_eq!(get(s, c, l), Value::UNDEFINED);
            }
            // Re-walking `a`'s path reuses the transitions and their table.
            let d = new_obj(s, t);
            for &k in common.iter().chain(&left) {
                set_int(s, d, k, 0);
            }
            assert_eq!(table_of(s, a), table_of(s, d));
        });
    }
}

/// Attributes stored in the table come back from lookup, and changing an
/// attribute (dictionary mode) keeps every other key findable.
#[test]
fn attributes_survive_tables_and_the_move_to_dictionary_mode() {
    let mut vm = vm(true);
    vm.with(|s, t| {
        let keys: Vec<_> = (0..20).map(|i| atom(s, t, &format!("a{i}"))).collect();
        let o = new_obj(s, t);
        let attrs = [Attrs(7), Attrs(2), Attrs(0), Attrs(5)];
        for (i, &k) in keys.iter().enumerate() {
            let v = RootedValue::Primitive(Value::from_i32(i as i32));
            assert!(object::define(s, o, k, v, attrs[i % 4]).unwrap());
        }
        let nogc = s.nogc();
        let sh = object::shape(nogc, o.get(nogc));
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(
                shape::lookup(nogc, sh, k.get(nogc)).unwrap().attrs,
                attrs[i % 4]
            );
        }
        // Deleting a middle property moves to dictionary mode.
        assert!(object::delete(s, o, keys[3]).unwrap());
        assert!(object::is_dictionary(s.nogc(), o.get(s.nogc())));
        let nogc = s.nogc();
        let sh = object::shape(nogc, o.get(nogc));
        for (i, k) in keys.iter().enumerate() {
            let p = shape::lookup(nogc, sh, k.get(nogc));
            if i == 3 {
                assert_eq!(p, None);
            } else {
                let p = p.unwrap();
                assert_eq!((p.slot, p.attrs), (i as u32, attrs[i % 4]), "a{i}");
            }
        }
    });
}

/// A dictionary object with many keys, churned by deletes and re-adds:
/// every live key is found, deleted ones never are, and a re-added key is
/// found once (its old index bucket points at a deleted entry).
#[test]
fn dictionary_lookups_after_churn() {
    for stress in [false, true] {
        let mut vm = vm(stress);
        vm.with(|s, t| {
            let n = if stress { 300 } else { 1000 };
            let keys: Vec<_> = (0..n).map(|i| atom(s, t, &format!("d{i}"))).collect();
            let o = new_obj(s, t);
            for (i, &k) in keys.iter().enumerate() {
                set_int(s, o, k, i as i32);
            }
            assert!(object::is_dictionary(s.nogc(), o.get(s.nogc())));
            let mut live = vec![true; n];
            for round in 0..3 {
                for (i, &k) in keys.iter().enumerate() {
                    if (i + round) % 3 == 0 {
                        assert!(object::delete(s, o, k).unwrap());
                        live[i] = false;
                    }
                }
                for (i, &k) in keys.iter().enumerate() {
                    if (i + round) % 5 == 0 && !live[i] {
                        set_int(s, o, k, -(i as i32));
                        live[i] = true;
                    }
                }
                s.collect();
                for (i, &k) in keys.iter().enumerate() {
                    let v = get(s, o, k);
                    assert_eq!(v.is_undefined(), !live[i], "d{i} round {round}");
                }
                let nogc = s.nogc();
                let own = object::own_keys(nogc, o.get(nogc));
                assert_eq!(own.len(), live.iter().filter(|&&l| l).count());
            }
        });
    }
}

/// The tracer reports a shape's key table, and every key in the table.
/// (Atoms are strong in Phase 1, D-038, so this checks the edges rather
/// than a key surviving only through them.)
#[test]
fn the_tracer_reports_key_table_edges() {
    let mut vm = vm(false);
    vm.with(|s, t| {
        let o = new_obj(s, t);
        let n = TABLE_MIN + 3;
        let keys: Vec<_> = (0..n).map(|i| atom(s, t, &format!("t{i}"))).collect();
        for (i, &k) in keys.iter().enumerate() {
            set_int(s, o, k, i as i32);
        }
        let table = table_of(s, o);
        assert_ne!(table, 0);
        let nogc = s.nogc();
        let sh = object::shape(nogc, o.get(nogc));
        let mut edges = Vec::new();
        ferrojs_vm::Tracer::default().trace(
            nogc.cage(),
            sh.offset(),
            nogc.cell_size(sh),
            &mut edges,
        );
        assert!(edges.contains(&table), "shape reports its table");
        let size = 16 + 16 * (nogc.cage().read_u64(table) >> 32) as u32;
        let mut table_edges = Vec::new();
        ferrojs_vm::Tracer::default().trace(nogc.cage(), table, size, &mut table_edges);
        for k in &keys {
            assert!(table_edges.contains(&k.get(nogc).offset()), "key in table");
        }
        assert_eq!(table_edges.len(), n as usize);
    });
}
