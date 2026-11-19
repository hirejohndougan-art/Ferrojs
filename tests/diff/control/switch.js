function classify(x) {
  switch (x) {
    case 1:
      return 'one';
    case '1':
      return 'string one';
    case 2:
    case 3:
      return 'two or three';
    default:
      return 'other';
  }
}
print(classify(1));
print(classify('1'));
print(classify(3));
print(classify(null));

function fallthrough(x) {
  const out = [];
  switch (x) {
    case 0:
      out.push('zero');
    case 1:
      out.push('one');
      break;
    default:
      out.push('default');
    case 2:
      out.push('two');
  }
  return out.join(' ');
}
print(fallthrough(0));
print(fallthrough(1));
print(fallthrough(2));
print(fallthrough(9));
