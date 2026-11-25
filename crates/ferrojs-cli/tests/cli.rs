//! The `ferrojs` binary end to end: output, uncaught errors, exit codes.

use std::process::{Command, Output};

fn ferrojs(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ferrojs"))
        .args(args)
        .output()
        .expect("runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn runs_a_file_and_prints() {
    let dir = std::env::temp_dir().join(format!("ferrojs-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("fib.js");
    std::fs::write(
        &file,
        "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }\n\
         for (let i = 0; i < 10; i++) print('fib(' + i + ') = ' + fib(i));\n\
         let o = { a: 1, b: 'two' }; o.c = o.a + 2; print(o.b + o.c);\n",
    )
    .unwrap();
    let out = ferrojs(&[file.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let lines: Vec<String> = text(&out.stdout).lines().map(String::from).collect();
    assert_eq!(lines.len(), 11);
    assert_eq!(lines[9], "fib(9) = 34");
    assert_eq!(lines[10], "two3");
    assert!(out.stderr.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn uncaught_errors_go_to_stderr_with_status_1() {
    let out = ferrojs(&["-e", "print('before'); undeclared; print('after')"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "before\n");
    let err = text(&out.stderr);
    assert!(err.contains("Uncaught ReferenceError"), "{err}");

    let out = ferrojs(&["-e", "throw 'boom'"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("Uncaught boom"));

    let out = ferrojs(&["-e", "function f() { return f() + 1 } f()"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("Uncaught RangeError"));
}

#[test]
fn syntax_errors_report_a_position_and_run_nothing() {
    let out = ferrojs(&["-e", "print(1);\nprint(2 +);"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "nothing runs");
    let err = text(&out.stderr);
    assert!(err.contains("SyntaxError"), "{err}");
    assert!(err.contains("line 2"), "{err}");
}

#[test]
fn unsupported_features_and_usage_errors_have_their_own_status() {
    let out = ferrojs(&["-e", "class A {}"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).contains("not supported"));

    assert_eq!(ferrojs(&[]).status.code(), Some(2));
    assert_eq!(ferrojs(&["-e"]).status.code(), Some(2));
    assert_eq!(ferrojs(&["--bogus"]).status.code(), Some(2));
    let missing = ferrojs(&["/nonexistent/ferrojs-test.js"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(ferrojs(&["--help"]).status.success());
}
