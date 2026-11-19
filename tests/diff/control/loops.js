let total = 0;
for (let i = 0; i < 10; i++) {
  if (i === 3) continue;
  if (i === 8) break;
  total += i;
}
print(total);

let n = 0;
while (n < 5) n++;
print(n);

do {
  n--;
} while (n > 2);
print(n);

outer: for (let i = 0; i < 3; i++) {
  for (let j = 0; j < 3; j++) {
    if (j === 2) continue outer;
    if (i === 2) break outer;
    print(i + ',' + j);
  }
}

const keys = [];
for (const k in { a: 1, b: 2, c: 3 }) keys.push(k);
print(keys.join());

const vals = [];
for (const v of [10, 20, 30]) vals.push(v * 2);
print(vals.join());

const fns = [];
for (let i = 0; i < 3; i++) fns.push(() => i);
print(fns.map((f) => f()).join());
