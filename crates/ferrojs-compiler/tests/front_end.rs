//! The front end accepts valid programs and rejects every kind of early
//! error the parser, the semantic pass and the goal symbol are responsible
//! for (D-018).

use ferrojs_compiler::{check, parse, Allocator, Goal};

fn ok(src: &str, goal: Goal) {
    if let Err(e) = check(src, goal) {
        panic!("expected {src:?} to parse as {goal:?}, got {e}");
    }
}

fn err(src: &str, goal: Goal) {
    assert!(
        check(src, goal).is_err(),
        "expected {src:?} to be rejected as {goal:?}"
    );
}

#[test]
fn valid_scripts() {
    for src in [
        "",
        "#!/usr/bin/env node\n1;",
        "var a = 1, b = [a, ...[2]], { c, d: [e] = [] } = {};",
        "function* g() { yield* g(); } async function f() { await 1; }",
        "class A { #x; static { this.y = 1; } get x() { return this.#x; } }",
        "label: for (const k in {}) { continue label; }",
        "x = /a(?<n>b)\\k<n>/u.test(`t${1}`) ?? 0n;",
        // Sloppy-mode only: `with`, octal literals, `yield` and `let` as names.
        "with ({}) { var yield = 010; } var let = 1;",
        // Annex B: HTML-like comments in scripts.
        "x = 1 <!-- comment\n--> also a comment\n",
    ] {
        ok(src, Goal::Script);
    }
}

#[test]
fn syntax_errors() {
    for src in [
        "var;",
        "if (",
        "a +;",
        "function () {}",
        "class { }",
        "`unterminated",
        "x = 'unterminated",
        "1 = 2;",
        "({a: 1} = 1)",
    ] {
        err(src, Goal::Script);
    }
}

#[test]
fn early_errors_from_scope_analysis() {
    for src in [
        "let x; let x;",
        "const y;",
        "let z; var z;",
        "{ function f() {} let f; }",
        "class C extends Object { constructor() { super(); } constructor() {} }",
        "function f() { super(); }",
        "class D { #p; m() { this.#q; } }",
        "break;",
        "lbl: lbl: ;",
        "function f(a, a) { 'use strict'; }",
        "(a, a) => 1",
        "new.target",
        "for (let i, j of []) ;",
    ] {
        err(src, Goal::Script);
    }
}

#[test]
fn strict_mode_errors() {
    for src in [
        "'use strict'; with ({}) {}",
        "'use strict'; var x = 010;",
        "'use strict'; var eval = 1;",
        "'use strict'; delete x;",
        "'use strict'; var yield;",
        "function f(a = 1) { 'use strict'; }",
    ] {
        err(src, Goal::Script);
    }
}

#[test]
fn regular_expression_early_errors() {
    for src in [
        "/(/;",
        "/a/gg;",
        "/\\k<n>/u;",
        "/(?<n>a)(?<n>b)/;",
        "/{1}/u;",
    ] {
        err(src, Goal::Script);
    }
}

#[test]
fn goals_differ() {
    // Module code is strict, has import/export and top-level await, and
    // forbids HTML-like comments.
    for src in [
        "import x from 'm'; export { x };",
        "export default function () {}",
        "await 1;",
        "import.meta.url;",
    ] {
        ok(src, Goal::Module);
        err(src, Goal::Script);
    }
    for src in [
        "with ({}) {}",
        "var await;",
        // In a module `<!--` is `<`, `!`, `--`, so test the closing form,
        // which only lexes as a comment in scripts.
        "x;\n--> comment",
        "export { missing };",
        "export let a; export let b; export { a as b };",
    ] {
        err(src, Goal::Module);
    }
    ok("var await;", Goal::Script);
}

#[test]
fn parse_keeps_the_ast_and_scopes() {
    let allocator = Allocator::default();
    let parsed = parse(
        &allocator,
        "let a = 1; function f(b) { return a + b; }",
        Goal::Script,
    )
    .expect("valid");
    assert_eq!(parsed.program.body.len(), 2);
    let scoping = parsed.semantic.scoping();
    assert!(scoping.symbols_len() >= 3, "a, f and b are symbols");
}

#[test]
fn errors_carry_a_message_and_an_offset() {
    let allocator = Allocator::default();
    let errors = parse(&allocator, "let q = 1;\nlet q = 2;", Goal::Script)
        .err()
        .expect("redeclaration is an early error");
    assert!(!errors.is_empty());
    let e = &errors[0];
    assert!(e.message.contains('q'), "message names the binding: {e}");
    let offset = e.offset.expect("oxc labels the span") as usize;
    assert!(offset < 22, "offset {offset} is inside the source");
}
