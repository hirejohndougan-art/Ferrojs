function counter() {
  let n = 0;
  return { inc: () => ++n, get: () => n };
}
const c = counter();
c.inc();
c.inc();
print(c.get());

function fib(n) {
  return n < 2 ? n : fib(n - 1) + fib(n - 2);
}
print(fib(20));

const compose = (...fns) => (x) => fns.reduceRight((acc, f) => f(acc), x);
print(compose((x) => x + 1, (x) => x * 2)(5));

function defaults(a, b = a * 2, ...rest) {
  return [a, b, rest.length].join();
}
print(defaults(1));
print(defaults(1, 5, 6, 7));
print(defaults.length);

function args() {
  return arguments.length;
}
print(args(1, 2, 3));

const obj = {
  name: 'obj',
  regular() {
    return this.name;
  },
  arrow: () => typeof this,
};
print(obj.regular());
print(obj.arrow());
print(obj.regular.call({ name: 'other' }));
const bound = obj.regular.bind({ name: 'bound' });
print(bound());

print((function () {
  'use strict';
  return this;
})());

var hoisted = typeof later;
function later() {}
print(hoisted);
