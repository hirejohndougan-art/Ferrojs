const o = { b: 1, a: 2, 2: 'two', 1: 'one', [Symbol.iterator]: null };
print(Object.keys(o).join());
o.c = 3;
delete o.b;
print(JSON.stringify(o));
print(['a' in o, 'b' in o, o.hasOwnProperty('c')].join(' '));

const proto = { greet() { return 'hi ' + this.name; } };
const child = Object.create(proto);
child.name = 'child';
print(child.greet());
print(Object.getPrototypeOf(child) === proto);
print(Object.keys(child).join());

const frozen = Object.freeze({ x: 1 });
frozen.x = 2;
print(frozen.x);
print(Object.isFrozen(frozen));

const withAccessor = {
  _v: 1,
  get v() { return this._v * 10; },
  set v(x) { this._v = x; },
};
withAccessor.v = 4;
print(withAccessor.v);

const d = Object.getOwnPropertyDescriptor(withAccessor, '_v');
print([d.writable, d.enumerable, d.configurable].join(' '));

const { a, ...rest } = { a: 1, b: 2, c: 3 };
print([a, JSON.stringify(rest)].join(' '));
const [x, , y = 9, ...zs] = [1, 2, undefined, 4, 5];
print([x, y, zs.join()].join(' '));
print(JSON.stringify({ ...{ p: 1 }, q: 2 }));
