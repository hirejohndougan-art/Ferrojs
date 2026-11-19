const m = new Map([['a', 1]]);
m.set('b', 2).set(NaN, 'nan').set(-0, 'zero');
print([m.size, m.get('b'), m.get(NaN), m.get(0)].join(' '));
print([...m.keys()].map(String).join());
m.delete('a');
print([m.has('a'), m.size].join(' '));

const s = new Set([1, 2, 2, 3, 1]);
print([s.size, [...s].join()].join(' '));
s.add(4);
print([...s].filter((x) => x % 2 === 0).join());

const wm = new WeakMap();
const key = {};
wm.set(key, 'v');
print([wm.get(key), wm.has({})].join(' '));

const sym = Symbol('tag');
const o = { [sym]: 1, plain: 2 };
print([Object.keys(o).length, Object.getOwnPropertySymbols(o).length].join(' '));
print([sym.toString(), sym.description].join(' '));
print(Symbol.for('k') === Symbol.for('k'));
