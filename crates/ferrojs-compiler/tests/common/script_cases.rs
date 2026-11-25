// Scripts shared by the compiler's toy-context tests and the
// interpreter's tests (task 1.7c): each runs in a fresh context, and the
// global `result` (or the error it throws) is compared with Node.js.
// Strings are compared as UTF-16 code units. Included with `include!`.

/// Scripts and globals (task 1.6e, D-042).
const SCRIPT_CASES: &[&[&str]] = &[
    &["var result = 1 + 2;"],
    &["result = 7;"],
    &["let a = 2; const b = 3; var result = a * b;"],
    &["function sq(x) { return x * x; } var result = sq(9);"],
    &["var result = f(); function f() { return 5; }"],
    &["var result; result = typeof_missing;"],
    &["'use strict'; var result = 1; undeclared = 2;"],
    &["var result = 1; undeclared = 2; result = undeclared + 1;"],
    &["var result = x; let x = 1;"],
    &["const c = 1; c = 2; var result = c;"],
    &["let n = 0; function inc() { n++; return n; } inc(); inc(); var result = inc() * 10 + n;"],
    &["var counter = 0; function bump() { counter += 2; } bump(); bump(); var result = counter;"],
    &["var result = 0; for (var i = 0; i < 5; i++) result += i; result = result * 100 + i;"],
    &["var result = 0; for (let i = 0; i < 5; i++) result += i;"],
    &["var result = 0; { let x = 4; result = x; } "],
    &["var result = 1; if (true) { var result = 2; }"],
    &["function f() { return g(); } function g() { return 42; } var result = f();"],
    &["function f() { 'use strict'; return this; } var result = f();"],
    &["var result = (function () { 'use strict'; return this === undefined; })();"],
    &["let result = 1;", "result = result + 1;"],
    &["var v = 1;", "let v = 2;"],
    &["let w = 1;", "var w = 2;"],
    &["let w = 1;", "let w = 2;"],
    &["implicit = 1;", "let implicit = 2; var result = implicit;"],
    &["const k = 5;", "var result = k * 2;"],
    &[
        "function f() { return later; }",
        "let later = 3;",
        "var result = f();",
    ],
    &[
        "function f() { return later; } var result = 0; try { f(); } catch (e) { result = 1; }",
        "let later = 3;",
    ],
    &["var result = 0; try { missing; } catch (e) { result = 9; }"],
    &["var result = 0; function f(a) { if (a) { return result = a; } } f(4);"],
    &["var result = (() => { let s = 0; for (let i = 1; i <= 10; i++) s += i; return s; })();"],
    &["var NaNs = NaN; var result = NaNs !== NaNs && undefined === void 0;"],
    // An implicit global stays configurable through a later `var`, so a
    // later `let` may shadow it (task 1.7c).
    &["x = 1;", "var x;", "let x = 2; var result = x;"],
    &["var a;", "'use strict'; a = 1; var result = a;"],
    &["implicit = 1;", "'use strict'; implicit = 2; var result = implicit;"],
    &["const c = 1;", "'use strict'; c = 2;"],
    &["var result = 1;", "'use strict'; (() => { late = 2; })(); let late;"],
    &["let late2 = (() => { throw 1; })();"],
    &["var y = 1;", "let y = 2;"],
    &["function z() {}", "let z = 2;"],
    &["let r = 1; { let r = 2; } var result = r; r = 3; result = result * 10 + r;"],
    &["var a1 = 1, a2 = 2, a3 = 3, a4 = 4, a5 = 5, a6 = 6, a7 = 7, a8 = 8, a9 = 9; var result = a1 + a9;"],
    &["let b1 = 1, b2 = 2, b3 = 3, b4 = 4, b5 = 5, b6 = 6, b7 = 7, b8 = 8, b9 = 9, b10 = 10, b11 = 11, b12 = 12, b13 = 13, b14 = 14, b15 = 15, b16 = 16, b17 = 17;", "var result = b1 + b9 + b17;"],
];

/// Strings (task 1.6f): literals, templates, `typeof`, concatenation,
/// Number::toString, StringToNumber, comparisons and equality.
const STRING_CASES: &[&[&str]] = &[
    &["var result = 'a' + 'b';"],
    &["var result = 'x' + 1 + 2;"],
    &["var result = 1 + 2 + 'x';"],
    &["var result = '3' + 4 * '5';"],
    &["var result = true + '1' + null + undefined;"],
    &["var result = `${0.1 + 0.2}|${1e21}|${1e-7}|${123e-20}|${-0}|${NaN}|${Infinity}|${-Infinity}`;"],
    &["var result = `${2 ** 53}|${5e-324}|${1.7976931348623157e308}|${100}|${0.000001}|${1234.5678}|${-1.5e-9}`;"],
    &["var result = `${999999999999999900000}|${123456789012345680000}|${1.5e300}|${4.35}|${-12345678}|${0.1 * 3}|${1 / 3}`;"],
    &["var result = typeof 'a' + typeof 1 + typeof undefined + typeof null + typeof true + typeof function () {} + typeof missing;"],
    &["var result = typeof NaN + typeof (undefined) + typeof (missing) + typeof (() => 1);"],
    &["var result = typeof x; let x;"],
    &["let r = typeof y; var y = 1; var result = r + typeof y;"],
    &["var result = (function () { let z = 's'; return typeof z + typeof q; })();"],
    &["var result = ('a' < 'b') + ',' + ('B' < 'a') + ',' + ('10' < '9') + ',' + ('10' < 9) + ',' + ('abc' >= 'ab') + ',' + ('' <= '') + ',' + ('a' > 'b') + ',' + ('a' < 1);"],
    &["var result = ('\\u{1F600}' < '\\uFFFF') + '' + ('\\uFFFF' > '\\u{10000}');"],
    &["var result = `${'1' == 1}${'' == 0}${' \\n1\\t' == 1}${'0x10' == 16}${'a' == 'a'}${'a' === 'a'}${'1' === 1}${true == '1'}${null == ''}${undefined == 'undefined'}${'NaN' == NaN}${'1e3' == 1000}${'a' != 'b'}${'a' !== 'a'}`;"],
    &["var result = ('6' * '7') + ('10' - 3) + ('8' / '2') + (+'  12  ') + (-'3') + ('5' % 3) + ('2' ** '3') + ('7' | 0) + (~'1') + ('1' << '4');"],
    &["var result = `${+''}|${+'0b101'}|${+'0o17'}|${+'-0x1'}|${+'Infinity'}|${+'-Infinity'}|${+'infinity'}|${+'1_0'}|${+'.5'}|${+'5.'}|${+'.'}|${+'+.5e1'}|${+'1e'}|${+'\\u00a0 7 \\ufeff'}`;"],
    &["var result = `${!''}${!'0'}${!' '}` + ('' ? 1 : 2) + ('x' && 'y') + ('' || 'z');"],
    &["var s = ''; for (let i = 0; i < 5; i++) s += i; var result = s;"],
    &["let a = 1, b = 'x'; var result = `${a}${b}` + `pre${a + 1}mid${b}post` + `` + `${null}${undefined}${true}`;"],
    &["let n = 0; var result = `${n++}-${n++}-${n}`;"],
    &["function f() { let s = 'a'; s = `${s}b${s}`; return s; } var result = f();"],
    &["let s = 'q'; s += `${s}!`; var result = s;"],
    &["var result = 'h\\u00e9llo \\u{1F600} ' + 'é' + `\\t${'\\x41'}`;"],
    &["var result; try { throw 'boom' + 1; } catch (e) { result = e; }"],
    &["throw 'uncaught';"],
    &["var result; switch ('b') { case 'a': result = 1; break; case 'b': result = 2; break; }"],
    &["var result = 'a\\tb\\n\\\\c\\'d\"';"],
    &["let s = '5'; s++; var result = s;"],
    &["let s = '5'; var result = s++ + s;"],
    &["let s = 'ab'; for (let i = 0; i < 8; i++) s = s + s; var result = s;"],
    &["var result = `line1
line2` + `a\\
b`;"],
    &["var w = 'v'; var result = (() => { var w = 'inner'; return `${w}:${typeof w}`; })() + w;"],
    // Strings made at run time compare by contents (task 1.7c).
    &["var a = 'a', result = `${(a + 'b') === 'ab'}${'ab' === a + 'b'}${(a + 'b') == 'ab'}${(a + 'b') !== 'ac'}${`${a}` === a}${(a + 1) == 'a1'}`;"],
    &["var k = 'b'; var result; switch ('a' + k) { case 'ab': result = 1; break; default: result = 2; }"],
    &["var s = 'x' + 'y'; var result = (s < 'xz') + ',' + (s >= 'xy') + ',' + (s > s + '') + ',' + (s + 1 > 'xy0');"],
    &["var result = ('1' + '0') * 2 + +('3' + '.5') + ((-1 + '') - 1);"],
    &["let parts = ''; for (let i = 0; i < 40; i++) parts = parts + i % 10; const result = parts;"],
    &["var result = '' + '' === '' && !('' + '') && !!('a' + '');"],
];

/// Default parameter values (task 1.12a, D-046): left-to-right
/// initialization, `undefined` triggers the default, parameter TDZ
/// (closures included), scoping of initializers, captured parameters.
const DEFAULT_CASES: &[&[&str]] = &[
    &["function f(a, b = 2) { return a * 10 + b; } var result = f(1) + ',' + f(1, 5) + ',' + f(1, undefined) + ',' + f(1, null);"],
    &["function f(a = 1, b = a + 1, c = a + b) { return `${a}${b}${c}`; } var result = f() + f(5) + f(5, 0) + f(undefined, 7);"],
    &["function f(a = b, b) { return a; } var result; try { f(); } catch (e) { result = 'tdz'; }"],
    &["function f(a = b, b) { return a; } var result = f(3);"],
    &["function f(a = a) { return a; } var result = f(4);"],
    &["function f(a = a) { return a; } f();"],
    &["function f(a = (b = 1), b) { return b; } f();"],
    &["function f(g = () => b, b = 2) { return g(); } var result = f();"],
    &["function f(g = () => b, h = g(), b = 2) { return h; } f();"],
    &["function f(a, g = () => a) { a = 9; return g(); } var result = f(1);"],
    &["function f(a = 1) { return () => a; } var result = f()() + f(2)();"],
    &["function f(a = 1, b = () => a) { a++; return b() + a; } var result = f(10);"],
    &["var x = 'outer'; function f(a = x) { var x = 'inner'; return a + x; } var result = f();"],
    &["var x = 'outer'; function f(g = () => x) { var x = 'inner'; return g() + x; } var result = f();"],
    &["function f(a = 1) { var a; return a; } var result = f() + f(2);"],
    &["function f(a = 1) { var a = 5; return a; } var result = f();"],
    &["let n = 0; function f(a = ++n, b = ++n) { return a * 10 + b; } var result = `${f()}|${f(0)}|${f(undefined, 0)}|${n}`;"],
    &["var calls = ''; function t(s) { calls += s; return s; } function f(a = t('a'), b = t('b')) { return a + b; } f('x'); f(undefined, 'y'); var result = calls;"],
    &["var result = ((a, b = a * 2) => a + b)(3);"],
    &["var result = ((a = 'p', b = `${a}q`) => b)();"],
    &["var result = (function g(n, acc = 1) { return n <= 1 ? acc : g(n - 1, acc * n); })(5);"],
    &["var result = (function g(a = g) { return typeof a; })();"],
    &["function f(a = 'use strict') { return a; } var result = f();"],
    &["function f(a = 1) { function a() {} return typeof a; } var result = f();"],
    &["function f(a, b = 1) { let c = a + b; return c; } var result = f(1) + f(2, 2);"],
    &["function f(a = (() => { throw 'boom'; })()) { return a; } var result = f(1); f();"],
    &["'use strict'; function f(a = this) { return a; } var result = f();"],
    &["function f(a = 1, b) { return b === undefined ? a : a + b; } var result = f(undefined, 2) + f();"],
    &["var arguments = 3; var result = (() => arguments)() + arguments;"],
];

/// Properties (task 1.7e, D-052): object literals, reads, writes,
/// compound and update assignments, `delete`, `in`, method calls, keys
/// through ToPropertyKey, string `length` and indices, and the errors.
/// Objects have no prototype yet, so no case touches an inherited name.
const PROPERTY_CASES: &[&[&str]] = &[
    &["var o = { a: 1, b: 'x' }; var result = o.a + o.b;"],
    &["var o = {}; o.x = 5; o['y'] = 6; var result = o.x * 10 + o.y;"],
    &["var o = { a: 1 }; var result = o.missing;"],
    &["var o = { a: 1, a: 2 }; var result = o.a;"],
    &["var k = 'dyn'; var o = { [k + 1]: 7 }; var result = o.dyn1;"],
    &["var o = { 0: 'zero', 1.5: 'x', 0x10: 'hex', 'two words': 'w' }; var result = o[0] + o['1.5'] + o[16] + o['two words'];"],
    &["var o = {}; o[1] = 'a'; o['1'] += 'b'; o[1.0] += 'c'; var result = o[1];"],
    &["var o = {}; o[-0] = 'z'; var result = o[0] + o['0'] + (o['-0'] === undefined);"],
    &["var o = {}; o[-1] = 'n'; o[1.5] = 'f'; o[4294967295] = 'big'; o[4294967294] = 'max'; var result = o['-1'] + o['1.5'] + o['4294967295'] + o['4294967294'];"],
    &["var o = {}; o[true] = 1; o[null] = 2; o[undefined] = 3; var result = o.true + o['null'] * 10 + o.undefined * 100;"],
    &["var o = { '01': 'lead', 1: 'one' }; var result = o['01'] + o[1] + (o['01'] === o[1]);"],
    &["var x = 3, y = 'q'; var o = { x, y }; var result = o.x + o.y;"],
    &["var o = { n: { m: { v: 42 } } }; var result = o.n.m.v;"],
    &["var o = { n: {} }; o.n.v = 1; o.n.v += 2; var result = o.n.v;"],
    &["var o = { c: 1 }; o.c++; ++o.c; o.c--; var result = o.c++ * 10 + o.c;"],
    &["var o = { c: '5' }; var a = o.c++; var result = typeof a + a + o.c;"],
    &["var o = { s: 'a' }; o.s += 'b'; o['s'] += 1; var result = o.s;"],
    &["var o = { a: 0, b: 1, c: null }; o.a ||= 'A'; o.b ||= 'B'; o.a &&= 'X'; o.c ??= 'C'; o.d ??= 'D'; var result = o.a + o.b + o.c + o.d;"],
    &["var o = { a: 1 }; var r = (o.a = 9); var result = r + o.a;"],
    &["var o = { a: 1, b: 2 }; var d = delete o.a; var result = d + ',' + o.a + ',' + ('a' in o) + ',' + ('b' in o);"],
    &["var o = { a: 1 }; var result = (delete o.zz) + ',' + (delete o['a']) + ',' + ('a' in o);"],
    &["var o = { a: 1 }; var result = delete 1;"],
    &["var o = {}; o[3] = 'x'; var result = ('3' in o) + ',' + (3 in o) + ',' + (delete o[3]) + ',' + (3 in o);"],
    &["var result = 'abc'.length + 'abc'[1] + 'abc'['2'] + 'abc'[3] + ''.length;"],
    &["var s = 'x'; for (let i = 0; i < 4; i++) s += s; var result = s.length + s[15];"],
    &["var result = `${'\\u00e9t\\u00e9'.length}${'\\u{1F600}'.length}`;"],
    &["var result = (delete 'abc'.length) + ',' + (delete 'abc'[0]) + ',' + (delete 'abc'[5]) + ',' + (delete 'abc'.foo) + ',' + (delete (5).x);"],
    &["'use strict'; var result = delete 'abc'.length;"],
    &["'use strict'; var result = delete 'abc'[9];"],
    &["var s = 'abc'; s.x = 1; s[0] = 'z'; var result = s + s.length + s[0];"],
    &["'use strict'; var s = 'abc'; s.x = 1;"],
    &["'use strict'; var n = 5; n.x = 1;"],
    &["var o; o.x;"],
    &["var o = null; o.x = 1;"],
    &["var o = undefined; o[0];"],
    &["var o = null; delete o.x;"],
    &["var result = 'a' in 'abc';"],
    &["var result = 'a' in 5;"],
    &["var calls = ''; function t(s, v) { calls += s; return v; } var o = {}; t('o', o)[t('k', 'p')] = t('v', 1); var result = calls + o.p;"],
    &["var calls = ''; function t(s, v) { calls += s; return v; } var o = { p: 1 }; t('o', o)[t('k', 'p')] += t('v', 2); var result = calls + o.p;"],
    &["var calls = ''; function t(s, v) { calls += s; return v; } var result; try { t('o', null)[t('k', 'p')] = t('v', 1); } catch (e) { result = calls; }"],
    &["var calls = ''; function t(s, v) { calls += s; return v; } var o = { [t('a', 'x')]: t('b', 1), [t('c', 'y')]: t('d', 2) }; var result = calls + o.x + o.y;"],
    &["var o = { a: 1 }; var p = o; o = { a: 2 }; p.a = 3; var result = o.a * 10 + p.a;"],
    &["var o = { a: 1 }; o = { b: o }; var result = o.b.a;"],
    &["var o = { a: 'x' }, q = { a: 'y' }; o[(o = q, 'a')] = 'z'; var result = o.a + q.a;"],
    &["var o = {}; for (var i = 0; i < 20; i++) o['k' + i] = i; var s = 0; for (var i = 0; i < 20; i++) s += o['k' + i]; var result = s;"],
    &["var o = {}; for (var i = 0; i < 300; i++) o[i * 3] = i; var result = o[0] + o[297] + o[897] + (o[1] === undefined);"],
    &["var o = { a: 1, b: 2, c: 3 }; delete o.b; o.d = 4; var result = o.a + o.c + o.d + ('b' in o);"],
    &["'use strict'; var o = { v: 3, get() { return this.v; } }; var result = o.get() + o['get']();"],
    &["'use strict'; var o = { v: 3, f: function () { return this.v * 2; } }; var result = o.f();"],
    &["'use strict'; var o = { n: 0, inc() { this.n++; return this; } }; o.inc().inc().inc(); var result = o.n;"],
    &["'use strict'; function f() { return this; } var o = { f }; var result = o.f() === o;"],
    &["var o = { f: (a, b) => a + b }; var result = o.f(2, 3);"],
    &["'use strict'; var o = { d: 0, down(n) { if (n === 0) return this.d; this.d++; return this.down(n - 1); } }; var result = o.down(5000);"],
    &["var o = { n: 1 }; var result = o.n();"],
    &["var o = {}; o.missing();"],
    &["var o = { a: { b: 2 } }; var result = typeof o + typeof o.a + typeof o.a.b + typeof o.zz;"],
    &["var o = {}, p = {}; var result = (o == p) + ',' + (o === o) + ',' + (o != p) + ',' + !o;"],
    &["function mk(v) { return { v: v }; } var a = mk(1), b = mk(2); var result = a.v + b.v;"],
    &["var o = { a: 1 }; function f() { return o.a; } o.a = 5; var result = f();"],
    &["let o = { k: 1 }; { let o = { k: 2 }; o.k = 3; } var result = o.k;"],
    &["var o = { x: 1 }; var r = o.x++ + o.x + ++o.x; var result = r * 100 + o.x;"],
    &["var o = { '': 'empty' }; var result = o[''];"],
    &["var o = {}; o.a = o; o.a.a.a.b = 7; var result = o.b;"],
    &["var o = {}; var result = 0; try { o.x.y = 1; } catch (e) { result = 1; }"],
    &["var o = { v: 1 }; with_like: { o.v = 2; break with_like; } var result = o.v;"],
];

fn node_results(cases: &[&[&str]]) -> Option<Vec<String>> {
    let mut script = String::from(
        "const vm=require('vm');\
         function show(v){if(v===undefined)return'undefined';if(v===null)return'null';\
         if(typeof v==='string')return's:'+Array.from({length:v.length},(_,i)=>v.charCodeAt(i).toString(16)).join(',');\
         if(typeof v==='boolean')return String(v);if(typeof v==='function')return'function';\
         if(Number.isNaN(v))return'NaN';\
         const d=new DataView(new ArrayBuffer(8));d.setFloat64(0,v);\
         return'n:'+d.getBigUint64(0).toString(16).padStart(16,'0');}\n\
         function run(srcs){const c=vm.createContext({});\
         for(const s of srcs){try{vm.runInContext(s,c);}catch(e){\
         return 'throw:'+(e&&typeof e==='object'&&'name' in e?e.name:show(e));}}\
         try{return show(vm.runInContext('result',c));}catch(e){return 'no result';}}\nconst out=[];\n",
    );
    for case in cases {
        let json: Vec<String> = case.iter().map(|s| format!("{:?}", s)).collect();
        script.push_str(&format!("out.push(run([{}]));\n", json.join(",")));
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
