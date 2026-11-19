function* range(start, end) {
  for (let i = start; i < end; i++) yield i;
  return 'done';
}
print([...range(0, 5)].join());

const it = range(0, 2);
let r;
while (!(r = it.next()).done) print(r.value);
print(r.value);

function* echo() {
  let received = yield 'first';
  while (true) received = yield 'got ' + received;
}
const e = echo();
print(e.next('ignored').value);
print(e.next('a').value);
print(e.next('b').value);
print(e.return('stop').value);
print(e.next().done);

function* delegate() {
  yield* [1, 2];
  yield* range(3, 5);
}
print([...delegate()].join());
