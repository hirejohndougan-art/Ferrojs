// Promise jobs run after the script, in a defined order.
print('sync 1');
Promise.resolve().then(() => print('micro 1'));
Promise.resolve().then(() => print('micro 2'));
Promise.reject(new Error('handled')).catch((e) => print('caught ' + e.message));
(async () => {
  print('async start');
  await null;
  print('after await');
  const v = await Promise.all([1, Promise.resolve(2)]);
  print('all ' + v.join());
})();
new Promise((resolve) => {
  print('executor');
  resolve('resolved');
}).then((v) => print(v));
print('sync 2');
