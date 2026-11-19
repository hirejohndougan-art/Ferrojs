class Shape {
  #id;
  static count = 0;
  constructor(name) {
    this.name = name;
    this.#id = ++Shape.count;
  }
  get id() {
    return this.#id;
  }
  area() {
    return 0;
  }
  toString() {
    return `${this.name}#${this.#id} area=${this.area()}`;
  }
}

class Rect extends Shape {
  constructor(w, h) {
    super('rect');
    this.w = w;
    this.h = h;
  }
  area() {
    return this.w * this.h;
  }
}

class Square extends Rect {
  constructor(s) {
    super(s, s);
    this.name = 'square';
  }
  static of(s) {
    return new this(s);
  }
}

print(String(new Shape('blob')));
print(String(new Rect(2, 3)));
const sq = Square.of(4);
print(String(sq));
print([sq instanceof Rect, sq instanceof Shape].join(' '));
print(Shape.count);
print(typeof Shape);
print(Object.getPrototypeOf(Square) === Rect);

try {
  Shape();
} catch (e) {
  print(e.name);
}
