print(JSON.stringify({ a: [1, 'x', null, true], b: { c: undefined } }));
print(JSON.stringify([undefined, function () {}, NaN, -0]));
print(JSON.stringify({ a: 1, b: [1, 2] }, null, 2));
print(JSON.stringify('quote " and \n newline'));
const parsed = JSON.parse('{"x":[1,2,{"y":"z"}],"n":-1.5e2}');
print([parsed.x[2].y, parsed.n].join(' '));
print(JSON.parse('[1,2,3]', (k, v) => (typeof v === 'number' ? v * 10 : v)).join());
try {
  JSON.parse('{bad');
} catch (e) {
  print(e.name);
}

print([Math.max(1, 5, 3), Math.min(), Math.max()].join(' '));
print([Math.floor(-1.5), Math.ceil(-1.5), Math.round(-1.5), Math.round(2.5)].join(' '));
print([Math.trunc(-4.7), Math.sign(-3), Math.abs(-7)].join(' '));
print([Math.sqrt(16), Math.cbrt(27), Math.hypot(3, 4)].join(' '));
print(Math.pow(2, 0.5) === Math.SQRT2);
print([parseInt('42px'), parseInt('ff', 16), parseFloat('3.14abc')].join(' '));
print([Number('12'), Number.isInteger(5.0), Number.isNaN('x')].join(' '));
