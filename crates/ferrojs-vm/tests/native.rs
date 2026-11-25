//! Host functions (task 1.9, D-054): JavaScript calls Rust closures
//! installed as globals; arguments and results cross as owned values, so
//! the heap stays sound under GC stress.

use std::cell::RefCell;
use std::rc::Rc;

use ferrojs_compiler::compile_script_source;
use ferrojs_core::gc::CxOptions;
use ferrojs_core::heap::HeapOptions;
use ferrojs_vm::interp::Outcome;
use ferrojs_vm::native::DefineError;
use ferrojs_vm::{HostValue, Vm, VmOptions};

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

/// A VM with `print` collecting into the returned log.
fn with_print(stress: bool) -> (Vm, Rc<RefCell<Vec<String>>>) {
    let mut vm = vm(stress);
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    vm.define_native(
        "print",
        Box::new(move |_this, args| {
            let line = args.first().map_or(String::new(), |a| a.to_string());
            sink.borrow_mut().push(line);
            Ok(HostValue::Undefined)
        }),
    )
    .unwrap();
    (vm, log)
}

/// Runs `src` as a script: global `r` afterwards (scripts have no
/// completion values yet), or the thrown value, copied out.
fn run(vm: &mut Vm, src: &str) -> Result<HostValue, HostValue> {
    let f = compile_script_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    let base = vm.stack_top();
    let out = vm.run_script(&f).expect("loads");
    assert_eq!(vm.stack_top(), base, "the stack unwinds after a run");
    match out {
        Outcome::Returned(_) => Ok(vm
            .global("r")
            .map_or(HostValue::Undefined, |v| vm.host_value(v))),
        Outcome::Threw(v, _) => Err(vm.host_value(v)),
        other => panic!("{src}: {other:?}"),
    }
}

#[test]
fn print_sees_every_kind_of_value() {
    for stress in [false, true] {
        let (mut vm, log) = with_print(stress);
        run(
            &mut vm,
            "print(1); print(-0); print(0.1 + 0.2); print(1e21); print(NaN_ = 0/0);\
             print('a' + 'b'); print(true); print(null); print(undefined);\
             print(); print({}); print(function () {}); print(print);\
             let s = ''; for (let i = 0; i < 5; i++) s += i; print(s);",
        )
        .unwrap();
        assert_eq!(
            *log.borrow(),
            [
                "1",
                "0",
                "0.30000000000000004",
                "1e+21",
                "NaN",
                "ab",
                "true",
                "null",
                "undefined",
                "",
                "[object Object]",
                "function () { [native code] }",
                "function () { [native code] }",
                "01234",
            ]
        );
    }
}

#[test]
fn host_functions_return_values_throw_and_see_this() {
    for stress in [false, true] {
        let mut vm = vm(stress);
        vm.define_native(
            "add",
            Box::new(|_, args| match args {
                [HostValue::Number(a), HostValue::Number(b)] => Ok((a + b).into()),
                _ => Err("add wants two numbers".into()),
            }),
        )
        .unwrap();
        vm.define_native(
            "greet",
            Box::new(|_, args| Ok(format!("hello, {}", args[0]).into())),
        )
        .unwrap();
        vm.define_native("kind", Box::new(|this, _| Ok(format!("{this:?}").into())))
            .unwrap();
        vm.define_native("bad", Box::new(|_, _| Ok(HostValue::Object)))
            .unwrap();
        assert_eq!(
            run(&mut vm, "var r = add(2, 3) * 2"),
            Ok(HostValue::Number(10.0))
        );
        assert_eq!(
            run(&mut vm, "var r = greet('x') + '!'"),
            Ok(HostValue::String("hello, x!".into()))
        );
        // A tail call to a host function returns its result.
        assert_eq!(
            run(
                &mut vm,
                "'use strict'; function f(n) { return add(n, 1); } var r = f(41)"
            ),
            Ok(HostValue::Number(42.0))
        );
        assert_eq!(
            run(
                &mut vm,
                "var r; try { add(1) } catch (e) { r = 'caught: ' + e }"
            ),
            Ok(HostValue::String("caught: add wants two numbers".into()))
        );
        assert_eq!(
            run(&mut vm, "add('a')"),
            Err(HostValue::String("add wants two numbers".into()))
        );
        assert_eq!(
            run(&mut vm, "let o = { kind }; var r = o.kind()"),
            Ok(HostValue::String("Object".into()))
        );
        assert_eq!(
            run(&mut vm, "var r = typeof add + ' ' + typeof greet"),
            Ok(HostValue::String("function function".into()))
        );
        // Objects can't come back from the host side: a clean throw.
        assert!(run(&mut vm, "bad()").is_err());
    }
}

#[test]
fn host_functions_are_ordinary_global_properties() {
    let (mut vm, log) = with_print(false);
    // Writable and configurable: scripts can wrap or replace them.
    run(
        &mut vm,
        "var p = print; print = function (x) { p('[' + x + ']') }; print(1);",
    )
    .unwrap();
    assert_eq!(*log.borrow(), ["[1]"]);
    // A non-configurable `var` of the same name refuses a host function.
    run(&mut vm, "var taken = 1;").unwrap();
    assert_eq!(
        vm.define_native("taken", Box::new(|_, _| Ok(HostValue::Undefined))),
        Err(DefineError::Refused)
    );
    assert_eq!(run(&mut vm, "var r = taken"), Ok(HostValue::Number(1.0)));
    // A `let` shadows a host function, as for any global property.
    let (mut vm, log) = with_print(false);
    assert_eq!(
        run(&mut vm, "let print = 5; var r = print"),
        Ok(HostValue::Number(5.0))
    );
    assert!(log.borrow().is_empty());
}

#[test]
fn host_values_survive_collections_while_calls_run() {
    // Under GC stress every allocation collects: the strings passed in and
    // the ones made from results must stay valid.
    let (mut vm, log) = with_print(true);
    vm.define_native(
        "twice",
        Box::new(|_, args| Ok(format!("{0}{0}", args[0]).into())),
    )
    .unwrap();
    run(
        &mut vm,
        "let a = {}; for (let i = 0; i < 20; i++) a[i] = twice('x' + i);\
         let t = ''; for (let i = 0; i < 20; i++) t += a[i]; print(t.length); print(a[19]);",
    )
    .unwrap();
    assert_eq!(*log.borrow(), ["100", "x19x19"]);
}

#[test]
fn the_interrupt_flag_stops_a_host_call() {
    let mut vm = vm(false);
    let flag = vm.interrupt.clone();
    vm.define_native(
        "stop",
        Box::new(move |_, _| {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(HostValue::Undefined)
        }),
    )
    .unwrap();
    let f = compile_script_source("stop(); stop();").unwrap();
    // The first call sets the flag; the second is refused.
    assert!(matches!(vm.run_script(&f).unwrap(), Outcome::Terminated));
}

#[test]
fn replacing_a_host_function_reuses_its_slot_and_objects_throw_a_type_error() {
    let mut vm = vm(false);
    for n in [1.0, 2.0] {
        vm.define_native("which", Box::new(move |_, _| Ok(HostValue::Number(n))))
            .unwrap();
    }
    assert_eq!(run(&mut vm, "var r = which()"), Ok(HostValue::Number(2.0)));
    vm.define_native("obj", Box::new(|_, _| Err(HostValue::Function)))
        .unwrap();
    let f = compile_script_source("obj()").unwrap();
    assert!(matches!(
        vm.run_script(&f).unwrap(),
        Outcome::Threw(_, Some(ferrojs_bytecode::ErrorKind::HostResult))
    ));
}
