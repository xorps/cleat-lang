# Language Specification (draft)

Status: draft. Decisions marked **(provisional)** are defaults chosen to unblock
work and are expected to be revisited.

## 1. Design principles

- **Structural by default, nominal on request.** Tuples, records, unions, and
  refinements compare by shape. `nominal` types and `enum`s compare by name.
- **Subtyping is the glue.** Unions, `Never`, refinements, and enum variants are
  all expressed through one subtyping relation.
- **Refinements are decidable.** Predicates stay within a fragment an SMT solver
  decides quickly (QF-UFLIA plus measures).
- **Refinements are erased; tags are not.** Anything a program can discriminate
  at runtime carries a runtime tag.

## 2. Types

```
τ ::= Never                         -- bottom, empty union
    | Int | Float | Bool | Str      -- primitives (nominal)
    | N[τ, ...]                     -- nominal type or enum, by name
    | N.V                           -- enum variant type
    | (τ₁, ..., τₙ)                 -- tuple
    | { l₁: τ₁, ..., lₙ: τₙ }       -- exact record
    | { l₁: τ₁, ..., lₙ: τₙ, .. }   -- open record ("at least these fields")
    | τ₁ | τ₂                       -- untagged union
    | τ where φ                     -- refinement (binder is `self`)
    | (τ₁, ..., τₙ) -> τ            -- function
    | α                             -- type variable
```

### 2.1 Tuples

`(A, B)` is sugar for the exact record `{0: A, 1: B}`. All product rules are
stated once, for records. `()` is the empty record, the unit type.

### 2.2 Records

- Records are **exact** by default: `{a: Int}` has exactly the field `a`.
- `{a: Int, ..}` is **open**: it has at least `a`.
- Fields are immutable by default. A `mut` field is **invariant** (see §3).
- Anonymous record types may appear anywhere a type may, including signatures.

### 2.3 Never

`Never` has no values. It is the identity of `|`, a subtype of every type, and
the return type of diverging functions. `match` on a `Never` with zero arms is
well typed.

### 2.4 Nominal types

```rust
nominal UserId = Int
nominal Port   = Int where 0 <= self && self < 65536
```

- A nominal type is distinct from its underlying type and from every other
  nominal type, even when the shapes are the same.
- Construction is explicit: `Port(x)`. If the underlying type is refined, the
  construction site must prove the refinement (§5). After that, consumers of
  `Port` get the refinement for free (it is an invariant).
- Unwrapping is explicit: `p.0` or `p as Int`.
- There is no implicit inheritance between nominal types.

### 2.5 Aliases

`type T = ...` is a purely structural alias. `T` and its definition are the
same type.

### 2.6 Disjoint unions (enums)

```rust
enum Shape {
  Circle { r: Float where r > 0 },
  Rect   (Float, Float),
  Empty,
}
```

- `enum` declares a nominal, closed, tagged sum.
- Every variant `Shape.V` is a type in its own right, and `Shape.V <: Shape`.
- `Shape` ≡ `Shape.Circle | Shape.Rect | Shape.Empty`. This lets untagged
  unions select subsets of variants:
  `fn f(s: Shape.Circle | Shape.Rect) -> Float`.

### 2.7 Untagged unions

`A | B` is a set-like structural union. Unions are normalized (§4).

A union is **discriminable** at runtime if each member can be told apart by tag
or by an executable predicate. Pattern matching that splits a union is only
allowed on discriminable unions:

- Primitives, nominal types, and enum variants carry runtime tags.
- Exact records are discriminated by field set and then by field types.
- Refined members are discriminated by evaluating their predicate, so
  refinement predicates must be executable (§5.1).
- A bare type variable `α` is never discriminable. Use an `enum` instead.

### 2.8 Refinements

```rust
type Nat         = Int where self >= 0
type NonEmpty[T] = List[T] where len(self) > 0
type Range       = { lo: Int, hi: Int } where self.lo <= self.hi
fn div(a: Int, b: Int where b != 0) -> Int
```

In parameter position, `b: Int where b != 0` may name the parameter instead of
`self`, and later parameters' predicates may mention earlier ones.

### 2.9 Generics **(provisional)**

- Type parameters: `List[T]`, `fn id[T](x: T) -> T`.
- Variance is **declared** on nominal types and enums (`enum List[+T]`,
  `nominal Sink[-T]`). Undeclared means invariant. Aliases are expanded, so they
  need no variance.
- No higher-kinded types in v1.

## 3. Subtyping

`τ <: σ` is the least relation closed under:

| Rule | Statement |
|---|---|
| Refl | `τ <: τ` |
| Bot | `Never <: τ` |
| Union-L | `τ₁ \| τ₂ <: σ` iff `τ₁ <: σ` and `τ₂ <: σ` |
| Union-R | `τ <: σ₁ \| σ₂` if `τ <: σ₁` or `τ <: σ₂` (after normalization) |
| Nominal | `N[τ̄] <: N[σ̄]` per the declared variance of each parameter |
| Variant | `N.V <: N` |
| Record-exact | `{l̄: τ̄} <: {l̄: σ̄}` iff the label sets are the same and `τᵢ <: σᵢ` for each field |
| Record-open | `{l̄: τ̄, m̄: ρ̄}` (exact or open) `<: {l̄: σ̄, ..}` iff `τᵢ <: σᵢ` |
| Mut field | a `mut` field requires `τᵢ <: σᵢ` and `σᵢ <: τᵢ` |
| Func | `(ā) -> r <: (b̄) -> s` iff `bᵢ <: aᵢ` and `r <: s` |
| Refine | `τ where φ <: σ where ψ` iff `τ <: σ` and `φ ⇒ ψ` is valid (SMT) |
| Refine-weaken | `τ where φ <: τ` |

An exact record is never a subtype of an open record with *different* field
types, and an open record is never a subtype of an exact one.

## 4. Normalization

Applied to types before subtyping and exhaustiveness checks:

1. Flatten nested unions; deduplicate members.
2. Remove `Never` from unions.
3. Remove any member that is a subtype of another member
   (`Nat | Int` → `Int`).
4. Collapse uninhabited types to `Never`:
   a record or tuple with a `Never` field; `τ where false`; any refinement whose
   predicate the solver proves unsatisfiable.
5. Expand aliases.

## 5. Refinement logic

### 5.1 Predicate language

Predicates `φ` are limited to:

- integer and real **linear** arithmetic, comparisons, `&&`, `||`, `!`, `==`
- booleans
- field and tuple projection on `self` and in-scope parameters
- **measures**: pure, total, structurally recursive functions marked `@measure`
  (for example `len`, `size`), encoded as uninterpreted functions with
  axioms on constructors

No user-written quantifiers. Every predicate must also be **executable**,
because predicates drive union discrimination (§2.7) and gradual checks (§5.3).

### 5.2 Verification conditions

The checker emits a VC at every:

- subsumption point (argument passing, return, annotated `let`)
- construction of a nominal type whose underlying type is refined
- enum variant construction with refined fields

Path conditions from `if`, `match`, and earlier `let` bindings are added as
hypotheses. VCs are discharged by Z3 or CVC5.

### 5.3 Unproven VCs **(provisional)**

- Default: an unproven VC is a **compile error**, with the failing VC and a
  counterexample model if one is available.
- `--gradual`: an unproven VC becomes an inserted runtime check that panics
  when it fails, and the compiler emits a warning.

## 6. Narrowing and exhaustiveness

- `match` arms and `if` conditions narrow the scrutinee type. A type-test arm
  `x: T => ...` removes `T` from the remaining union. A boolean condition adds
  a hypothesis for the refinement checker.
- A `match` is exhaustive when the remaining scrutinee type normalizes to
  `Never`.
- Intersections are used **internally** by the checker to represent narrowed
  types, but there is no surface `&` syntax in v1. **(provisional)**

```rust
fn safe_div(a: Int, b: Int) -> Int | Str {
  if b == 0 { "div by zero" } else { div(a, b) }  // b: Int where b != 0
}

fn area(s: Shape.Circle | Shape.Rect) -> Float {
  match s {
    Shape.Circle { r } => 3.14159 * r * r,
    Shape.Rect (w, h)  => w * h,
  }                                               // exhaustive: Empty excluded
}
```

## 7. Checking pipeline

1. **Parse.**
2. **Desugar:** tuples to records, parameter-named refinements to `self`
   refinements, alias expansion.
3. **Base checking:** bidirectional checking with refinements erased.
   Top-level functions need annotations; local `let`s are inferred.
4. **Normalization** (§4).
5. **Refinement checking:** generate VCs (§5.2) and discharge them with SMT.
   Liquid-style inference of local refinements is optional, not in v1.
6. **Codegen:** erase refinements, keep runtime tags for primitives, nominal
   types, and enum variants. A nominal type may be unboxed when the compiler
   proves it never appears in a discriminated union.

## 8. Open questions

- Whether to add surface intersection types.
- Whether variance on nominal types should be inferred instead of declared.
- Module system and visibility (needed for nominal types to be truly opaque).
- Effects and errors: is `Result`-via-union (`T | Err`) the idiom?
