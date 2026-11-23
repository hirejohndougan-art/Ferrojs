//! The bytecode compiler, part 1a (D-033): every compiled function passes
//! the verifier, and running it on the toy context gives exactly what
//! Node.js gives for the same call.

use ferrojs_bytecode::testing::{
    run, run_program, run_with_handlers, Outcome, ToyCtx, ToyFunction,
};
use ferrojs_bytecode::{disassemble, verify, verify_handlers, ErrorKind};
use ferrojs_compiler::{compile_function_source, CompileError, CompiledFunction, Constant};
use ferrojs_value::{Kind, Value};

fn compile(src: &str) -> CompiledFunction {
    let f = compile_function_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    check_tree(src, &f);
    f
}

/// Every function in the tree passes the verifier.
fn check_tree(src: &str, f: &CompiledFunction) {
    let checked = verify(&f.code, &f.limits())
        .and_then(|()| verify_handlers(&f.code, &f.handlers, &f.limits()));
    if let Err(e) = checked {
        panic!(
            "{src}: compiled code fails verification: {e:?}\n{}\n{:?}",
            disassemble(&f.code).unwrap_or_default(),
            f.handlers
        );
    }
    for g in &f.functions {
        check_tree(src, g);
    }
}

/// The function tree as a flat toy program; the root is index 0.
fn flatten(f: &CompiledFunction, cx: &mut ToyCtx, out: &mut Vec<ToyFunction>) -> usize {
    let index = out.len();
    out.push(ToyFunction::default());
    let functions = f.functions.iter().map(|g| flatten(g, cx, out)).collect();
    out[index] = ToyFunction {
        code: f.code.clone(),
        handlers: f.handlers.clone(),
        constants: f.constant_values(|s| cx.intern(s)),
        limits: f.limits(),
        params: f.params,
        functions,
    };
    index
}

fn call(f: &CompiledFunction, args: &[f64]) -> Outcome {
    call_toy(f, args).0
}

fn call_toy(f: &CompiledFunction, args: &[f64]) -> (Outcome, ToyCtx) {
    let mut program = Vec::new();
    let mut cx = ToyCtx::default();
    flatten(f, &mut cx, &mut program);
    let args: Vec<Value> = args.iter().map(|&a| Value::number(a)).collect();
    let out = run_program(&program, 0, &args, &mut cx, 10_000_000);
    (out, cx)
}

fn show(v: Value) -> String {
    match v.kind() {
        Kind::Undefined => "undefined".into(),
        Kind::Null => "null".into(),
        Kind::Bool(b) => b.to_string(),
        _ => match v.as_number() {
            Some(d) if d.is_nan() => "NaN".into(),
            Some(d) => format!("n:{:016x}", d.to_bits()),
            None => format!("{v:?}"),
        },
    }
}

fn show_outcome((o, cx): (Outcome, ToyCtx)) -> String {
    match o {
        Outcome::Returned(v) => show(v),
        // The toy has no error objects; it records which error was thrown.
        Outcome::Threw(v) => match cx.last_error {
            Some(
                ErrorKind::UninitializedBinding
                | ErrorKind::UnresolvableReference
                | ErrorKind::InvalidAssignmentTarget,
            ) => "throw:ReferenceError".into(),
            Some(ErrorKind::GlobalRedeclaration) => "throw:SyntaxError".into(),
            Some(ErrorKind::ConstAssignment | ErrorKind::NotCallable) => "throw:TypeError".into(),
            Some(ErrorKind::StackOverflow) => "throw:RangeError".into(),
            None => format!("throw:{}", show(v)),
        },
        Outcome::StepLimit => "step limit".into(),
    }
}

/// Functions and argument lists, checked against Node.js.
const CASES: &[(&str, &[&[f64]])] = &[
    ("function f(a, b) { return a + b * 2 - 1; }", &[&[1.0, 2.0], &[2147483647.0, 1.0], &[0.5, -0.25]]),
    ("function f(a, b) { return a / b; }", &[&[1.0, 0.0], &[-1.0, 0.0], &[0.0, 0.0], &[6.0, 3.0], &[1.0, 3.0]]),
    ("function f(a, b) { return a % b; }", &[&[7.0, 3.0], &[-7.0, 3.0], &[-7.0, 7.0], &[5.5, 2.0], &[1.0, 0.0], &[-2147483648.0, -1.0]]),
    ("function f(a, b) { return a ** b; }", &[&[2.0, 10.0], &[1.0, f64::NAN], &[-1.0, f64::INFINITY], &[0.0, -1.0], &[-8.0, 1.0 / 3.0]]),
    ("function f(a, b) { return [a & b, a | b, a ^ b]; }", &[]),
    ("function f(a, b) { return (a & b) + (a | b) * 3 + (a ^ b) * 7; }", &[&[12.0, 10.0], &[-1.0, 4294967296.0], &[1e21, 3.7], &[f64::NAN, 5.0]]),
    ("function f(a, b) { return (a << b) + (a >> b) + (a >>> b); }", &[&[1.0, 31.0], &[-1.0, 0.0], &[-16.0, 2.0], &[5.0, 33.0], &[-1.0, -1.0]]),
    ("function f(a) { return [-a, +a, ~a]; }", &[]),
    ("function f(a) { return -a; }", &[&[0.0], &[-0.0], &[-2147483648.0], &[5.0]]),
    ("function f(a) { return ~a + !a; }", &[&[0.0], &[-1.0], &[f64::NAN], &[2147483648.0]]),
    ("function f(a, b) { return (a < b) + 2 * (a > b) + 4 * (a <= b) + 8 * (a >= b); }", &[&[1.0, 2.0], &[2.0, 1.0], &[1.0, 1.0], &[f64::NAN, 1.0], &[-0.0, 0.0]]),
    ("function f(a, b) { return (a == b) + 2 * (a != b) + 4 * (a === b) + 8 * (a !== b); }", &[&[1.0, 1.0], &[1.0, 2.0], &[f64::NAN, f64::NAN], &[0.0, -0.0]]),
    ("function f(a) { return (a == null) + 2 * (a == undefined) + 4 * (null == undefined) + 8 * (null === undefined) + 16 * (true == 1) + 32 * (false == null); }", &[&[0.0]]),
    ("function f(a) { return true + 1 + null + (undefined + 1 !== undefined + 1); }", &[&[0.0]]),
    ("function f(a, b) { return a && b; }", &[&[0.0, 5.0], &[3.0, 5.0], &[f64::NAN, 1.0]]),
    ("function f(a, b) { return a || b; }", &[&[0.0, 5.0], &[3.0, 5.0], &[-0.0, 0.0]]),
    ("function f(a) { var u; return (u ?? a) + (null ?? 1) + (0 ?? 9); }", &[&[4.0]]),
    ("function f(a) { return a > 0 ? 'p' : 'n'; }", &[]),
    ("function f(a) { return a > 0 ? 1 : a < 0 ? -1 : 0; }", &[&[5.0], &[-5.0], &[0.0], &[f64::NAN]]),
    ("function f(n) { var s = 0; for (var i = 1; i <= n; i++) s += i; return s; }", &[&[0.0], &[10.0], &[100000.0]]),
    ("function f(n) { let s = 0, i = 0; while (i < n) { i++; if (i % 3 === 0) continue; s += i; } return s; }", &[&[10.0], &[0.0]]),
    ("function f(n) { let s = 0; do { s = s * 2 + 1; n--; } while (n > 0); return s; }", &[&[0.0], &[5.0], &[40.0]]),
    ("function f(n) { let c = 0; outer: for (let i = 0; i < n; i++) { for (let j = 0; j < n; j++) { if (j > i) continue outer; if (i + j > 10) break outer; c++; } } return c; }", &[&[3.0], &[20.0]]),
    ("function f(n) { let r = 0; block: { if (n > 1) break block; r = 7; } return r; }", &[&[0.0], &[5.0]]),
    ("function f(a) { if (a) { return 1; } else if (a === 0) { return 2; } return 3; }", &[&[5.0], &[0.0], &[f64::NAN]]),
    ("function f(a) { let x = 1; { let x = 2; a = a + x; } return a + x; }", &[&[10.0]]),
    ("function f(a, b) { a += b; a -= 1; a *= 3; a /= 2; a %= 7; a **= 2; a <<= 1; a >>= 1; a >>>= 0; a &= 255; a |= 256; a ^= 1; return a; }", &[&[5.0, 3.0], &[-9.0, 0.5]]),
    ("function f(a, b) { a ||= b; b &&= a; let u; u ??= a + b; return u * 10 + a; }", &[&[0.0, 3.0], &[2.0, 3.0]]),
    ("function f(a) { let b = a++; let c = ++a; let d = a--; let e = --a; return [b, c, d, e]; }", &[]),
    ("function f(a) { let b = a++; let c = ++a; let d = a--; let e = --a; return b + c * 10 + d * 100 + e * 1000 + a * 10000; }", &[&[1.0], &[2147483647.0], &[-0.5]]),
    ("function f(a) { let t = true; t++; let n = null; n--; return t * 100 + n; }", &[&[0.0]]),
    ("function f(a) { a = (a = 2) + a; return a; }", &[&[9.0]]),
    ("function f(a) { var b = 1; b = a + (b = 10) + b; return b; }", &[&[5.0]]),
    ("function f(a) { let x = a; x = x && (x = 0, 7); return x; }", &[&[1.0], &[0.0]]),
    ("function f(a) { let x = a; x += (x = 3); return x; }", &[&[4.0]]),
    ("function f(a) { return (a, a + 1, a * 2); }", &[&[21.0]]),
    ("function f(a) { return void a; }", &[&[1.0]]),
    ("function f(a) { return undefined === void 0 && NaN !== NaN && Infinity > 1e308; }", &[&[0.0]]),
    ("function f(a, b) { if (a > b) throw a - b; return b - a; }", &[&[5.0, 3.0], &[3.0, 5.0]]),
    ("function f(a, a) { return a; }", &[&[1.0, 2.0]]),
    ("function f(a, b, c) { return c; }", &[&[1.0, 2.0]]),
    ("function f(a) { 'use strict'; let big = 1.5e300; return big * big + 0.1 + 1e-7; }", &[&[0.0]]),
    ("function f(n) { var x; if (n) { var x = 5; } return x; }", &[&[0.0], &[1.0]]),
    ("function f(n) { let r = 0; for (let i = 0; i < 3; i++) { let k; if (i === 1) k = 5; r = r * 10 + (k === undefined ? 1 : k); } return r; }", &[&[0.0]]),
    // switch (1.6b)
    ("function f(a) { let r = 0; switch (a) { case 1: r += 1; case 2: r += 2; break; case 3: r += 3; default: r += 10; case 4: r += 4; } return r; }", &[&[1.0], &[2.0], &[3.0], &[4.0], &[5.0], &[f64::NAN]]),
    ("function f(a) { switch (a) { case 0: return 1; case -0: return 2; } return 3; }", &[&[0.0], &[-0.0], &[1.0]]),
    ("function f(a) { switch (a) { case 1: return 1; } return 0; }", &[&[1.0], &[2.0]]),
    ("function f(a) { switch (a) { default: return 7; } }", &[&[1.0]]),
    ("function f(a) { switch (a) {} return 5; }", &[&[1.0]]),
    ("function f(a) { let n = 0; switch (a++) { case (n += 1, 1): n += 10; case (n += 100, 2): n += 1000; } return n * 10 + a; }", &[&[1.0], &[2.0], &[3.0]]),
    ("function f(n) { let s = 0; for (let i = 0; i < n; i++) { switch (i % 3) { case 0: continue; case 1: s += i; break; default: s += 100; } s += 1000; } return s; }", &[&[7.0]]),
    ("function f(n) { let s = 0; out: for (let i = 0; i < n; i++) { switch (i) { case 3: break out; default: s += i; } } return s; }", &[&[10.0]]),
    ("function f(a) { lbl: switch (a) { case 1: { break lbl; } case 2: return 2; } return 9; }", &[&[1.0], &[2.0]]),
    // TDZ (1.6b): run-time checks where order isn't provable.
    ("function f(a) { if (a) { return x; } let x = 1; return x; }", &[&[1.0], &[0.0]]),
    ("function f(a) { let x = x + 1; return x; }", &[&[0.0]]),
    ("function f(a) { const c = a > 0 ? c : 2; return c; }", &[&[1.0], &[-1.0]]),
    ("function f(a) { { if (a) x = 5; let x = 1; return x; } }", &[&[1.0], &[0.0]]),
    ("function f(a) { switch (a) { case 0: let y = 1; case 1: return y; } return 9; }", &[&[0.0], &[1.0], &[2.0]]),
    ("function f(a) { switch (a) { case 0: const k = 4; return k; default: return 5; } }", &[&[0.0], &[1.0]]),
    ("function f(n) { let s = 0; for (let i = 0; i < n; i++) { if (i === 2) { s += y; } let y = i * 10; s += y; } return s; }", &[&[2.0], &[3.0]]),
    ("function f(a) { let r = 0; for (let i = 0; i < 3; i++) { let z; if (i === 1) z = a; r = r * 10 + (z ?? 7); } return r; }", &[&[4.0]]),
    ("function f(a) { { a = a + 1; } let x = a; return x; }", &[&[1.0]]),
    ("function f(a) { let q = 1; switch (a) { case 0: return q; } return -q; }", &[&[0.0], &[1.0]]),
    // const assignment (1.6b): TypeError, after the right side runs.
    ("function f(a) { const c = 1; c = 2; return c; }", &[&[0.0]]),
    ("function f(a) { const c = a; c ||= 5; return c; }", &[&[1.0], &[0.0]]),
    ("function f(a) { const c = a; c &&= 5; return c; }", &[&[1.0], &[0.0]]),
    ("function f(a) { const c = a; c ??= 5; return c; }", &[&[1.0]]),
    ("function f(a) { const c = 1; c += 1; return c; }", &[&[0.0]]),
    ("function f(a) { const c = 1; c++; return c; }", &[&[0.0]]),
    ("function f(a) { const c = 1; return --c; }", &[&[0.0]]),
    ("function f(a) { const c = 1; return c--; }", &[&[0.0]]),
    ("function f(a) { 'use strict'; const c = 1; c = 2; }", &[&[0.0]]),
    ("function f(a) { if (a) { c = 5; } const c = 1; c = 3; }", &[&[1.0], &[0.0]]),
    // try / catch / finally (1.6c)
    ("function f(a) { try { if (a) throw a; return 1; } catch (e) { return e * 10; } }", &[&[0.0], &[3.0]]),
    ("function f(a) { let r = 0; try { r = 1; throw 5; } catch { r += 10; } return r; }", &[&[0.0]]),
    ("function f(a) { let r = 0; try { r = 1; } finally { r += 10; } return r; }", &[&[0.0]]),
    ("function f(a) { try { return a; } finally { a = 99; } }", &[&[4.0]]),
    ("function f(a) { try { return a; } finally { return 7; } }", &[&[4.0]]),
    ("function f(a) { try { throw a; } finally { return 7; } }", &[&[4.0]]),
    ("function f(a) { try { throw a; } finally { a = 1; } }", &[&[4.0]]),
    ("function f(a) { try { throw a; } catch (e) { throw e + 1; } finally { a = 1; } }", &[&[4.0]]),
    ("function f(a) { let r = 0; try { try { throw 1; } finally { r += 10; } } catch (e) { r += e * 100; } return r; }", &[&[0.0]]),
    ("function f(a) { let r = 0; try { try { throw 1; } catch (e) { r += e; throw 2; } finally { r += 10; } } catch (e) { r += e * 100; } return r; }", &[&[0.0]]),
    ("function f(n) { let s = 0; for (let i = 0; i < n; i++) { try { if (i === 2) continue; if (i === 4) break; s += i; } finally { s += 100; } } return s; }", &[&[10.0], &[3.0]]),
    ("function f(n) { let s = 0; outer: for (let i = 0; i < n; i++) { for (let j = 0; j < n; j++) { try { try { if (j === 1) continue outer; if (i === 3) break outer; s += 1; } finally { s += 10; } } finally { s += 100; } } } return s; }", &[&[5.0]]),
    ("function f(a) { let s = 0; for (;;) { try { return s; } finally { s = 5; break; } } return s + 1; }", &[&[0.0]]),
    ("function f(a) { let s = 0; try { try { return 1; } finally { s += 1; } } finally { s += 10; a = s; } }", &[&[0.0]]),
    ("function f(a) { let s = 0; try { try { return 1; } finally { s += 1; } } finally { return s + 10; } }", &[&[0.0]]),
    ("function f(a) { let s = 0; lbl: { try { break lbl; } finally { s = 3; } s = 9; } return s; }", &[&[0.0]]),
    ("function f(a) { let r = 0; try { r = 1; } catch (e) { r = 2; } finally { r *= 10; } return r; }", &[&[0.0]]),
    ("function f(a) { let r; try { r = x; let x = 1; } catch (e) { r = -1; } return r; }", &[&[0.0]]),
    ("function f(a) { const c = 1; try { c = 2; } catch { return 'caught' === 'caught' ? 5 : 6; } }", &[]),
    ("function f(a) { const c = 1; try { c = 2; } catch (e) { return a + 5; } }", &[&[1.0]]),
    ("function f(a) { try { } finally { a += 1; } try { } catch (e) { a = 0; } return a; }", &[&[1.0]]),
    ("function f(n) { let s = 0; for (let i = 0; i < n; i++) { try { switch (i) { case 1: continue; case 2: break; default: s += i; } s += 10; } finally { s += 100; } } return s; }", &[&[4.0]]),
    ("function f(e) { try { throw 1; } catch (e) { e += 5; } return e; }", &[&[3.0]]),
    // functions, closures and calls (1.6d)
    ("function f(a) { function sq(x) { return x * x; } return sq(a) + sq(2); }", &[&[3.0]]),
    ("function f(a) { const add = (x, y) => x + y; return add(a, 1) * add(2, 3); }", &[&[4.0]]),
    ("function f(a) { function g(x, y) { return y; } return g(a) === undefined && g(1, 2, 3) === 2; }", &[&[1.0]]),
    ("function f(n) { function fact(k) { return k <= 1 ? 1 : k * fact(k - 1); } return fact(n); }", &[&[5.0], &[20.0]]),
    ("function f(n) { var fib = function me(k) { return k < 2 ? k : me(k - 1) + me(k - 2); }; return fib(n); }", &[&[15.0]]),
    ("function f(a) { let count = 0; const inc = () => { count += a; return count; }; inc(); inc(); return inc() * 100 + count; }", &[&[2.0]]),
    ("function f(a) { function counter() { let c = 0; return () => ++c; } const x = counter(), y = counter(); x(); x(); y(); return x() * 10 + y(); }", &[&[0.0]]),
    ("function f(a) { function mk(k) { return (j) => k * 10 + j; } return mk(a)(3) + mk(7)(1); }", &[&[2.0]]),
    ("function f(a) { let g1, g2; for (let i = 0; i < 2; i++) { if (i === 0) g1 = () => i; else g2 = () => i; } return g1() * 10 + g2(); }", &[&[0.0]]),
    ("function f(a) { let g1, g2; for (var i = 0; i < 2; i++) { if (i === 0) g1 = () => i; else g2 = () => i; } return g1() * 10 + g2(); }", &[&[0.0]]),
    ("function f(a) { let g1, g2; for (let i = 0; i < 3; i++) { let k = i * 2; if (i === 0) g1 = () => k + i; if (i === 2) g2 = () => k + i; } return g1() * 100 + g2(); }", &[&[0.0]]),
    ("function f(a) { let g; for (let i = 0; i < 3; i++) { g = () => i; i++; } return g(); }", &[&[0.0]]),
    ("function f(a) { const g = () => x; try { g(); } catch (e) { a += 100; } let x = 5; return g() + a; }", &[&[1.0]]),
    ("function f(a) { const g = () => { c = 2; }; const c = 1; try { g(); } catch (e) { return c * 7; } }", &[&[0.0]]),
    ("function f(a) { let r = 0; { let z = a; const h = () => z * 2; r = h(); } return r; }", &[&[21.0]]),
    ("function f(a) { function g() { throw a + 1; } try { g(); } catch (e) { return e * 2; } }", &[&[4.0]]),
    ("function f(a) { function g() { try { throw 1; } finally { a += 10; } } try { g(); } catch (e) { return a + e; } }", &[&[4.0]]),
    ("function f(a) { function g(n) { return n > 0 ? g(n - 1) + 1 : 0; } return g(a); }", &[&[500.0]]),
    ("function f(a) { function g(n) { return g(n + 1); } return g(0); }", &[&[0.0]]),
    ("function f(a) { let x = 1; return x(); }", &[&[0.0]]),
    ("function f(a) { 'use strict'; function loop(n, acc) { return n === 0 ? acc : loop(n - 1, acc + n); } return loop(a, 0); }", &[&[10.0]]),
    ("function f(a) { let s = 0; const add = (k) => { s += k; }; for (let i = 0; i < a; i++) add(i); return s; }", &[&[10.0]]),
    ("function f(a) { const g = function h() { h = 5; return h; }; return g() === g; }", &[&[0.0]]),
    ("function f(a) { 'use strict'; const g = function h() { h = 5; }; g(); }", &[&[0.0]]),
    ("function f(a) { switch (a) { case 0: let v = 1; return (() => v)(); case 1: return (() => v)(); } }", &[&[0.0], &[1.0]]),
    ("function f(a) { try { throw 3; } catch (e) { const g = () => e * 2; return g(); } }", &[&[0.0]]),
    ("function f(a) { let r = 0; outer: for (let i = 0; i < 4; i++) { const g = () => i; for (let j = 0; j < 4; j++) { const h = () => j; if (h() > g()) continue outer; if (g() === 3) break outer; r += g() * 10 + h(); } } return r; }", &[&[0.0]]),
    ("function f(a) { let r = 0; for (let i = 0; i < 3; i++) { const g = () => i; try { if (i === 1) continue; r += g(); } finally { r += 100; } } return r; }", &[&[0.0]]),
    ("function f(a) { function g() { return h(); } function h() { return a * 3; } return g(); }", &[&[5.0]]),
    ("function f(a) { return g(); function g() { return a + 1; } }", &[&[1.0]]),
    ("function f(a) { var g = 1; function g() {} return g; }", &[&[0.0]]),
    ("function f(a) { return ((x) => (y) => (z) => x * 100 + y * 10 + z)(1)(2)(3); }", &[&[0.0]]),
];

fn node_results() -> Option<Vec<String>> {
    let mut script = String::from(
        "function show(v){if(v===undefined)return'undefined';if(v===null)return'null';\
         if(typeof v==='boolean')return String(v);if(Number.isNaN(v))return'NaN';\
         const d=new DataView(new ArrayBuffer(8));d.setFloat64(0,v);\
         return'n:'+d.getBigUint64(0).toString(16).padStart(16,'0');}\n\
         const out=[];\n",
    );
    for (src, calls) in CASES {
        for args in *calls {
            let args: Vec<String> = args
                .iter()
                .map(|a| {
                    if a.is_nan() {
                        "NaN".into()
                    } else if a.is_infinite() {
                        if *a > 0.0 {
                            "Infinity".into()
                        } else {
                            "-Infinity".into()
                        }
                    } else if *a == 0.0 && a.is_sign_negative() {
                        "-0".into()
                    } else {
                        format!("{a:e}")
                    }
                })
                .collect();
            script.push_str(&format!(
                "out.push((()=>{{{src}\ntry{{return show(f({}));}}catch(e){{return'throw:'+(e instanceof Error?e.name:show(e));}}}})());\n",
                args.join(",")
            ));
        }
    }
    script.push_str("console.log(out.join('\\n'));\n");
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(
        String::from_utf8(out.stdout)
            .ok()?
            .lines()
            .map(str::to_owned)
            .collect(),
    )
}

#[test]
fn compiled_functions_match_node() {
    let Some(expected) = node_results() else {
        eprintln!("node is not on the PATH; skipping the Node comparison");
        return;
    };
    let mut i = 0;
    let mut checked = 0;
    for (src, calls) in CASES {
        if calls.is_empty() {
            continue;
        }
        let f = compile(src);
        for args in *calls {
            let got = show_outcome(call_toy(&f, args));
            assert_eq!(got, expected[i], "{src} with {args:?}");
            i += 1;
            checked += 1;
        }
    }
    assert_eq!(i, expected.len());
    assert!(checked > 80, "only {checked} calls checked");
}

#[test]
fn hand_checked_results() {
    let f =
        compile("function sum(n) { let s = 0; for (let i = 1; i <= n; i++) s += i; return s; }");
    assert_eq!(f.name, "sum");
    assert_eq!(f.params, 1);
    assert_eq!(call(&f, &[100.0]), Outcome::Returned(Value::from_i32(5050)));
    let f = compile("function f() {}");
    assert_eq!(call(&f, &[]), Outcome::Returned(Value::UNDEFINED));
    let f = compile("function f(x) { 'use strict'; return x; }");
    assert!(f.strict);
    // `x` itself, plus the temporary of the implicit `return undefined`:
    // reading a parameter needs no temporary of its own.
    assert_eq!(f.registers, 2);
    let f = compile("function f(x) { return 0.5 + x + 0.5 + 1e100; }");
    assert_eq!(
        f.constants,
        vec![Constant::Number(0.5), Constant::Number(1e100)],
        "constants are shared"
    );
}

#[test]
fn termination_is_not_catchable() {
    let f =
        compile("function f() { for (;;) { try { while (true) {} } catch (e) {} finally {} } }");
    assert!(!f.handlers.is_empty());
    let mut cx = ToyCtx::new(&f.limits());
    cx.interrupt_after = Some(1000);
    let out = run_with_handlers(&f.code, &f.handlers, &mut cx, 1_000_000);
    assert_eq!(out, Outcome::Threw(Value::NULL));
}

#[test]
fn loops_check_for_interrupts() {
    let f = compile("function f() { while (true) {} }");
    let mut cx = ToyCtx::new(&f.limits());
    cx.interrupt_after = Some(1000);
    assert_eq!(
        run(&f.code, &mut cx, 1_000_000),
        Outcome::Threw(Value::NULL)
    );
}

fn unsupported(src: &str) -> String {
    match compile_function_source(src) {
        Err(CompileError::Unsupported { what, .. }) => what,
        other => panic!("expected {src:?} to be unsupported, got {other:?}"),
    }
}

#[test]
fn everything_else_is_unsupported_not_miscompiled() {
    for src in [
        "function f() { return '\\uD800'; }",
        "function f() { return `a\\uDC00${1}`; }",
        "function f() { return f`x`; }",
        "function f() { if (1) { function g() {} } }",
        "function f() { return { a: 1 }.a; }",
        "function f() { return new f(); }",
        "function f(a, g = () => a) { var a = 2; }",
        "function f(a = 1, g = function () { return a; }) { function a() {} }",
        "function f() { return arguments; }",
        "function f() { return () => typeof arguments; }",
        "function f() { arguments = 1; }",
        "function f(a = arguments) {}",
        "function f(...r) {}",
        "function f({ a }) {}",
        "function f() { let [a] = [1]; }",
        "function f() { return {}; }",
        "function f() { return this; }",
        "function f(o) { return o.p; }",
        "function f() { try {} catch ({ e }) {} }",
        "function f() { try {} catch (e) { var e = 1; } }",
        "function f() { for (var k in {}) ; }",
        "function* f() {}",
        "async function f() {}",
        "function f() { return 1n; }",
    ] {
        unsupported(src);
    }
    assert!(matches!(
        compile_function_source("function f( {"),
        Err(CompileError::Syntax(_))
    ));
}

#[test]
fn length_counts_parameters_before_the_first_default() {
    for (src, params, length) in [
        ("function f() {}", 0, 0),
        ("function f(a, b) {}", 2, 2),
        ("function f(a, b = 1, c) {}", 3, 1),
        ("function f(a = 1) {}", 1, 0),
    ] {
        let f = compile(src);
        assert_eq!((f.params, f.length), (params, length), "{src}");
    }
}

#[test]
fn proper_tail_calls_keep_the_stack_flat() {
    // 100,000 calls deep: a RangeError without tail calls (the toy stops
    // at 10,000 frames), fine with them. Node.js doesn't implement proper
    // tail calls, so this one is checked by hand.
    let src = "function f(n) { 'use strict'; function loop(k, acc) { return k === 0 ? acc : loop(k - 1, acc + 1); } return loop(n, 0); }";
    let f = compile(src);
    assert_eq!(
        call(&f, &[100_000.0]),
        Outcome::Returned(Value::from_i32(100_000))
    );
    // Sloppy code has no tail calls; neither does a call inside `try`.
    for src in [
        "function f(n) { function loop(k, acc) { return k === 0 ? acc : loop(k - 1, acc + 1); } return loop(n, 0); }",
        "function f(n) { 'use strict'; function loop(k) { return k === 0 ? 0 : 1 + loop(k - 1); } return loop(n); }",
    ] {
        let f = compile(src);
        let (out, cx) = call_toy(&f, &[100_000.0]);
        assert!(matches!(out, Outcome::Threw(_)), "{src}");
        assert_eq!(cx.last_error, Some(ErrorKind::StackOverflow), "{src}");
    }
    // A return inside `try` isn't in tail position: the deepest frame
    // overflows and its handler sees the RangeError.
    let f = compile("function f(n) { 'use strict'; function loop(k) { try { return k === 0 ? 0 : loop(k - 1); } catch (e) { return -1; } } return loop(n); }");
    assert_eq!(
        call(&f, &[100_000.0]),
        Outcome::Returned(Value::from_i32(-1))
    );
}

#[test]
fn nested_functions_are_compiled_into_the_tree() {
    let f = compile("function f(a) { function g() { return () => a; } return g()(); }");
    assert_eq!(f.functions.len(), 1);
    assert_eq!(f.functions[0].name, "g");
    assert_eq!(f.functions[0].functions.len(), 1);
    assert_eq!(call(&f, &[7.0]), Outcome::Returned(Value::from_i32(7)));
}
