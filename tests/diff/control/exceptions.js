function f() {
  try {
    throw new RangeError('bad');
  } catch (e) {
    print(e.name);
    print(e instanceof RangeError);
    print(e instanceof Error);
    return 'from catch';
  } finally {
    print('finally runs');
  }
}
print(f());

function g() {
  try {
    return 'try';
  } finally {
    return 'finally wins';
  }
}
print(g());

try {
  null.x;
} catch (e) {
  print(e.name);
}

try {
  undefinedVariable;
} catch (e) {
  print(e.name);
}

try {
  throw 42;
} catch (e) {
  print([typeof e, e].join(' '));
}

try {
  try {
    throw new Error('inner');
  } finally {
    print('inner finally');
  }
} catch ({ message }) {
  print(message);
}
