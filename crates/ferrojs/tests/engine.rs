//! The embedding API (task 1.9): scripts share globals, host functions
//! round-trip values, errors come back with their kind, interrupts stop
//! runs, and all of it holds under GC stress.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use ferrojs::{Engine, Error, JsValue, Options};

fn engines() -> [Engine; 2] {
    [
        Engine::new().unwrap(),
        Engine::with_options(Options {
            gc_stress: true,
            ..Options::default()
        })
        .unwrap(),
    ]
}

#[test]
fn scripts_share_globals_and_results_come_back_owned() {
    for mut e in engines() {
        e.run("var n = 1; let s = 'a'; const c = true; function f(x) { return x * 2 }")
            .unwrap();
        e.run("n = f(n + 20); s = s + n; var o = { k: 1 }; var u;")
            .unwrap();
        assert_eq!(e.global("n"), Some(JsValue::Number(42.0)));
        assert_eq!(e.global("s"), Some(JsValue::String("a42".into())));
        assert_eq!(e.global("c"), Some(JsValue::Bool(true)));
        assert_eq!(e.global("f"), Some(JsValue::Function));
        assert_eq!(e.global("o"), Some(JsValue::Object));
        assert_eq!(e.global("u"), Some(JsValue::Undefined));
        assert_eq!(e.global("missing"), None);
    }
}

#[test]
fn host_functions_take_and_return_values() {
    for mut e in engines() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        e.register("record", move |args| {
            log.borrow_mut().extend(args.iter().cloned());
            Ok(JsValue::Number(args.len() as f64))
        })
        .unwrap();
        e.register("upper", |args| match args.first() {
            Some(JsValue::String(s)) => Ok(s.to_uppercase().into()),
            _ => Err("upper wants a string".into()),
        })
        .unwrap();
        e.run("var k = record(1, 'x', null, undefined, false, {}, record); var u = upper('ok');")
            .unwrap();
        assert_eq!(
            *seen.borrow(),
            [
                JsValue::Number(1.0),
                JsValue::String("x".into()),
                JsValue::Null,
                JsValue::Undefined,
                JsValue::Bool(false),
                JsValue::Object,
                JsValue::Function,
            ]
        );
        assert_eq!(e.global("k"), Some(JsValue::Number(7.0)));
        assert_eq!(e.global("u"), Some(JsValue::String("OK".into())));
        let err = e.run("upper(1)").unwrap_err();
        assert_eq!(
            err,
            Error::Uncaught {
                value: JsValue::String("upper wants a string".into()),
                name: None,
                message: None
            }
        );
        assert_eq!(err.to_string(), "Uncaught upper wants a string");
    }
}

#[test]
fn errors_carry_their_kind_and_phase() {
    let mut e = Engine::new().unwrap();
    let err = e.run("var a = 1;\n  a +;").unwrap_err();
    assert_eq!(err.name(), Some("SyntaxError"));
    match &err {
        Error::Syntax { position, .. } => assert_eq!(position.map(|p| p.0), Some(2)),
        other => panic!("{other:?}"),
    }
    // Nothing ran: `a` doesn't exist.
    assert_eq!(e.global("a"), None);

    let cases = [
        ("nope", "ReferenceError"),
        ("const k = 1; k = 2;", "TypeError"),
        ("var v = 1; v();", "TypeError"),
        ("function r() { return 1 + r() } r()", "RangeError"),
        ("null.x", "TypeError"),
    ];
    for (src, name) in cases {
        let err = e.run(src).unwrap_err();
        assert_eq!(err.name(), Some(name), "{src}: {err}");
        assert!(err.to_string().starts_with(&format!("Uncaught {name}: ")));
    }
    assert!(matches!(e.run("class A {}"), Err(Error::Unsupported(_))));
    // The engine still works after every kind of failure.
    e.run("var after = 'fine'").unwrap();
    assert_eq!(e.global("after"), Some(JsValue::String("fine".into())));
}

#[test]
fn the_interrupt_handle_terminates_a_running_script() {
    let mut e = Engine::new().unwrap();
    let stop = e.interrupt_handle();
    let flag = stop.clone();
    e.register("tick", move |_| {
        flag.store(true, Ordering::Relaxed);
        Ok(JsValue::Undefined)
    })
    .unwrap();
    assert_eq!(e.run("tick(); for (;;) {}"), Err(Error::Terminated));
    stop.store(false, Ordering::Relaxed);
    e.run("var ok = 1").unwrap();
    assert_eq!(e.global("ok"), Some(JsValue::Number(1.0)));
}
