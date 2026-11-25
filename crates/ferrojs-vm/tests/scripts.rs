//! Strings and globals in the interpreter (task 1.7c, D-050): the
//! compiler's Node.js-checked script cases (scripts and globals, strings,
//! default parameters) run on the real interpreter, with the heap verifier
//! on and again under GC stress, and must give Node's result. Plus what
//! only the interpreter has: string cells that aren't interned, the
//! global environment surviving collections between scripts.

use ferrojs_bytecode::ErrorKind;
use ferrojs_compiler::compile_script_source;
use ferrojs_core::gc::CxOptions;
use ferrojs_core::heap::HeapOptions;
use ferrojs_value::{Kind, Value};
use ferrojs_vm::interp::Outcome;
use ferrojs_vm::{Vm, VmOptions};

include!("../../ferrojs-compiler/tests/common/script_cases.rs");

fn vm(stress: bool) -> Vm {
    Vm::new(VmOptions {
        heap_size: 64 * ferrojs_core::cage::CHUNK_SIZE as u64,
        heap: HeapOptions {
            poison: true,
            verify: true,
        },
        cx: CxOptions { gc_stress: stress },
        stack_slots: 1 << 16,
    })
    .unwrap()
}

/// The same rendering as the Node.js side of the harness.
fn show(vm: &Vm, v: Value) -> String {
    if let Some(units) = vm.string_units(v) {
        let units: Vec<String> = units.iter().map(|u| format!("{u:x}")).collect();
        return format!("s:{}", units.join(","));
    }
    match v.kind() {
        Kind::Undefined => "undefined".into(),
        Kind::Null => "null".into(),
        Kind::Bool(b) => b.to_string(),
        Kind::Object(_) => "function".into(),
        _ => match v.as_number() {
            Some(d) if d.is_nan() => "NaN".into(),
            Some(d) => format!("n:{:016x}", d.to_bits()),
            None => format!("{v:?}"),
        },
    }
}

/// Runs each script in order on one VM, then reports `result`.
fn run_scripts(vm: &mut Vm, sources: &[&str]) -> String {
    for src in sources {
        let f = compile_script_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        let base = vm.stack_top();
        let out = vm.run_script(&f).expect("loads");
        assert_eq!(vm.stack_top(), base, "the stack unwinds after a run");
        match out {
            Outcome::Returned(_) => {}
            Outcome::Threw(v, e) => {
                return match e {
                    Some(
                        ErrorKind::UninitializedBinding
                        | ErrorKind::UnresolvableReference
                        | ErrorKind::InvalidAssignmentTarget,
                    ) => "throw:ReferenceError".into(),
                    Some(
                        ErrorKind::ConstAssignment
                        | ErrorKind::NotCallable
                        | ErrorKind::NullishBase
                        | ErrorKind::NotAnObject
                        | ErrorKind::PropertyRefused,
                    ) => "throw:TypeError".into(),
                    Some(ErrorKind::GlobalRedeclaration) => "throw:SyntaxError".into(),
                    Some(ErrorKind::StackOverflow) => "throw:RangeError".into(),
                    None => format!("throw:{}", show(vm, v)),
                }
            }
            other => return format!("{other:?}"),
        }
    }
    match vm.global("result") {
        Some(v) => show(vm, v),
        None => "no result".into(),
    }
}

fn match_node(cases: &[&[&str]]) {
    let Some(expected) = node_results(cases) else {
        eprintln!("node is not on the PATH; skipping the Node comparison");
        return;
    };
    assert_eq!(expected.len(), cases.len());
    for stress in [false, true] {
        for (case, want) in cases.iter().zip(&expected) {
            let got = run_scripts(&mut vm(stress), case);
            assert_eq!(&got, want, "{case:?} (gc stress: {stress})");
        }
    }
}

#[test]
fn scripts_match_node() {
    match_node(SCRIPT_CASES);
}

#[test]
fn strings_match_node() {
    match_node(STRING_CASES);
}

#[test]
fn default_parameters_match_node() {
    match_node(DEFAULT_CASES);
}

#[test]
fn properties_match_node() {
    match_node(PROPERTY_CASES);
}

fn text(vm: &Vm, v: Value) -> String {
    String::from_utf16(&vm.string_units(v).expect("a string")).unwrap()
}

#[test]
fn globals_and_strings_survive_collections_between_scripts() {
    let mut vm = vm(true);
    let scripts = [
        "var s = 'a'; let t = ''; const f = () => s + t; implicit = 'i' + 1;",
        "for (let i = 0; i < 20; i++) { s = s + i; t = `${t}${i % 3}`; }",
        "var result = f() + implicit;",
    ];
    for src in scripts {
        let f = compile_script_source(src).unwrap();
        assert!(matches!(vm.run_script(&f).unwrap(), Outcome::Returned(_)));
        vm.cx.collect();
    }
    let want = {
        let (mut s, mut t) = (String::from("a"), String::new());
        for i in 0..20 {
            s += &i.to_string();
            t += &(i % 3).to_string();
        }
        format!("{s}{t}i1")
    };
    let result = vm.global("result").unwrap();
    assert_eq!(text(&vm, result), want);
    let t = vm.global("t").unwrap();
    assert_eq!(text(&vm, t).len(), 20);
    assert!(matches!(vm.global("f").unwrap().kind(), Kind::Object(_)));
    assert!(vm.cx.collections() > 20, "stress mode collected");
}

#[test]
fn run_time_strings_are_fresh_cells_and_constants_are_atoms() {
    let mut vm = vm(false);
    let f = compile_script_source("var a = 'ab', b = 'a' + 'b', c = 'ab';").unwrap();
    vm.run_script(&f).unwrap();
    let (a, b, c) = (
        vm.global("a").unwrap(),
        vm.global("b").unwrap(),
        vm.global("c").unwrap(),
    );
    assert_eq!(a, c, "constants are interned");
    assert_ne!(a, b, "concatenation makes a new cell");
    assert_eq!(vm.string_units(a), vm.string_units(b));
    let f = compile_script_source("var result = a === b && a == b && !(a < b) && a <= b;").unwrap();
    vm.run_script(&f).unwrap();
    assert_eq!(vm.global("result"), Some(Value::TRUE));
}

#[test]
fn lexical_bindings_report_their_tdz_and_a_failed_script_keeps_its_declarations() {
    let mut vm = vm(false);
    // The declaration happens before the throw; the binding stays in its
    // TDZ for good (as in Node.js).
    let f = compile_script_source("let late = (() => { throw 1; })();").unwrap();
    assert_eq!(
        vm.run_script(&f).unwrap(),
        Outcome::Threw(Value::from_i32(1), None)
    );
    assert_eq!(vm.global("late"), Some(Value::UNINITIALIZED));
    let f = compile_script_source("late = 2;").unwrap();
    assert_eq!(
        vm.run_script(&f).unwrap(),
        Outcome::Threw(Value::UNDEFINED, Some(ErrorKind::UninitializedBinding))
    );
    let f = compile_script_source("var late;").unwrap();
    assert_eq!(
        vm.run_script(&f).unwrap(),
        Outcome::Threw(Value::UNDEFINED, Some(ErrorKind::GlobalRedeclaration))
    );
    assert_eq!(vm.global("missing"), None);
}

#[test]
fn many_lexical_bindings_grow_the_environment() {
    let mut vm = vm(true);
    let run = |vm: &mut Vm, src: &str| vm.run_script(&compile_script_source(src).unwrap());
    assert!(matches!(
        run(&mut vm, "let first = 'f' + 1;").unwrap(),
        Outcome::Returned(_)
    ));
    let decls: Vec<String> = (0..300).map(|i| format!("let g{i} = 'v' + {i};")).collect();
    assert!(matches!(
        run(&mut vm, &decls.join("\n")).unwrap(),
        Outcome::Returned(_)
    ));
    for i in [0, 7, 8, 63, 64, 299] {
        let v = vm.global(&format!("g{i}")).unwrap();
        assert_eq!(text(&vm, v), format!("v{i}"));
    }
    let first = vm.global("first").unwrap();
    assert_eq!(text(&vm, first), "f1", "values move with the environment");
    // Bindings declared before the environment grew are still in their
    // TDZ after it.
    let decls: Vec<String> = (0..400).map(|i| format!("h{i} = {i}")).collect();
    let src = format!("let h = h1, {};", decls.join(", "));
    assert_eq!(
        run(&mut vm, &src).unwrap(),
        Outcome::Threw(Value::UNDEFINED, Some(ErrorKind::UninitializedBinding))
    );
}

/// Property cases only the interpreter can run (the toy context has no
/// lone surrogates, shapes or elements stores).
const INTERP_PROPERTY_CASES: &[&[&str]] = &[
    &["var s = '\\u{1F600}'; var result = s[0] + '|' + s[1] + '|' + s.length;"],
    &["var o = {}; for (var i = 0; i < 200; i++) o['p' + i] = i; delete o.p7; o.p199 += 1; var s = 0; for (var i = 0; i < 200; i++) if ('p' + i in o) s += o['p' + i]; var result = s;"],
    &["var o = { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6 }; delete o.b; delete o.f; o.g = 7; var result = o.a + o.c + o.d + o.e + o.g + ('b' in o) + ('f' in o);"],
    &["var o = {}; o[4000000000] = 'far'; o[2] = 'near'; o[1e6] = 'mid'; var result = o[4e9] + o[2] + o[1000000] + (delete o[1e6]) + (1e6 in o);"],
    &["var objs = {}; for (let i = 0; i < 50; i++) objs[i] = { v: 'x' + i, n: { m: i } }; var s = ''; for (let i = 0; i < 50; i += 7) s += objs[i].v + objs[i].n.m; var result = s;"],
];

#[test]
fn interpreter_properties_match_node() {
    match_node(INTERP_PROPERTY_CASES);
}

#[test]
fn objects_survive_collections_between_scripts() {
    let mut vm = vm(true);
    let scripts = [
        "var o = { a: 'x' + 1, n: { s: 'deep' + 2 } }; var k = 'key' + 3;",
        "o[k] = { list: {} }; for (let i = 0; i < 30; i++) o[k].list[i] = 'e' + i;",
        "delete o.a; o.b = o.n; var result = o.b.s + o[k].list[29] + ('a' in o);",
    ];
    for src in scripts {
        let f = compile_script_source(src).unwrap();
        assert!(
            matches!(vm.run_script(&f).unwrap(), Outcome::Returned(_)),
            "{src}"
        );
        vm.cx.collect();
    }
    let result = vm.global("result").unwrap();
    assert_eq!(text(&vm, result), "deep2e29false");
}

#[test]
fn reading_an_unknown_computed_key_interns_nothing() {
    let mut vm = vm(false);
    let run = |vm: &mut Vm, src: &str| vm.run_script(&compile_script_source(src).unwrap());
    run(&mut vm, "var o = { a: 1 }; var k = 'never' + 'written';").unwrap();
    let atoms = vm.tables.atom_count();
    let out = run(
        &mut vm,
        "var r = o[k] === undefined && !(k in o) && delete o[k];",
    )
    .unwrap();
    assert!(matches!(out, Outcome::Returned(_)));
    assert_eq!(vm.global("r"), Some(Value::TRUE));
    // The script's own constants (`r`) may be new; `neverwritten` isn't.
    assert!(vm.tables.atom_count() <= atoms + 1);
    run(&mut vm, "o[k] = 2;").unwrap();
    assert!(vm.tables.atom_count() > atoms);
}

#[test]
fn inherited_object_prototype_names_fail_cleanly() {
    for src in [
        "var o = {}; var result = o.toString;",
        "var o = {}; var result = 'hasOwnProperty' in o;",
        "var o = {}; o.__proto__ = { a: 1 };",
        "var o = {}; var result = o.__proto__;",
        "var o = {}; o['__proto__'] = 1;",
    ] {
        let f = compile_script_source(src).unwrap();
        assert_eq!(
            vm(false).run_script(&f).unwrap(),
            Outcome::Threw(Value::UNDEFINED, None),
            "{src}"
        );
    }
}
