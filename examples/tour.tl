// A tour of the type system. Check with: cargo run -- check examples/tour.tl --vcs

// Structural aliases, including refinements.
type Nat   = Int where self >= 0
type Range = { lo: Int, hi: Int } where self.lo <= self.hi

// Nominal types: distinct from their underlying type; refinements become invariants.
nominal UserId = Int
nominal Port   = Int where 0 <= self && self < 65536

// Disjoint unions: every variant is also a type (`Shape.Circle <: Shape`).
enum Shape {
  Circle { r: Float where r > 0.0 },
  Rect(Float, Float),
  Empty,
}

// Refined parameter: callers must prove `b != 0`.
fn div(a: Int, b: Int where b != 0) -> Int { a / b }

// Path conditions discharge the obligation; the result is an untagged union.
fn safe_div(a: Int, b: Int) -> Int | Str {
  if b == 0 { "division by zero" } else { div(a, b) }
}

fn area(s: Shape) -> Float {
  match s {
    Shape.Circle { r } => 3.14159 * r * r,
    Shape.Rect(w, h) => w * h,
    Shape.Empty => 0.0,
  }
}

// A union of variants excludes `Empty` statically, so no arm is needed for it.
fn area_nonempty(s: Shape.Circle | Shape.Rect) -> Float {
  match s {
    Shape.Circle { r } => 3.14159 * r * r,
    Shape.Rect(w, h) => w * h,
  }
}

// Type-test patterns narrow an untagged union; the match is exhaustive once
// the remaining type is `Never`.
fn describe(x: Int | Str | (Int, Int)) -> Str {
  match x {
    n: Nat => "natural",
    n: Int => "negative",
    s: Str => s,
    (a, b) => "pair",
  }
}

// Needs real arithmetic reasoning: left pending for the SMT phase.
fn width(r: Range) -> Nat { r.hi - r.lo }

// Anonymous records in signatures.
fn name_of(p: { name: Str, age: Nat }) -> Str { p.name }

fn http() -> Port { Port(8080) }

fn user(id: Int) -> UserId { UserId(id) }

fn fail(msg: Str) -> Never { panic(msg) }

// An uninhabited type: a match with no arms is exhaustive.
type Impossible = (Int, Never)
fn absurd(x: Impossible) -> Int { match x {} }

fn main() -> Int {
  let p = http();
  let n: Nat = 5;
  let s = Shape.Circle { r: 2.0 };
  let a = area(s);
  let who = name_of({ name: "ada", age: 36 });
  safe_div(10, 2);
  div(10, n + 1)
}
