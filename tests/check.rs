use test_lang::check_source;
use test_lang::vc::VcStatus;

const PRELUDE: &str = r#"
type Nat = Int where self >= 0
nominal UserId = Int
nominal Port = Int where 0 <= self && self < 65536
enum Shape { Circle { r: Float where r > 0.0 }, Rect(Float, Float), Empty }
fn div(a: Int, b: Int where b != 0) -> Int { a / b }
"#;

fn ok(src: &str) -> test_lang::Outcome {
    let out = check_source(&format!("{PRELUDE}\n{src}"));
    let msgs: Vec<_> = out.errors.iter().map(|e| format!("{}: {}", e.span, e.msg)).collect();
    assert!(msgs.is_empty(), "expected no errors, got:\n{}", msgs.join("\n"));
    out
}

fn err(src: &str, needle: &str) {
    let out = check_source(&format!("{PRELUDE}\n{src}"));
    assert!(
        out.errors.iter().any(|e| e.msg.contains(needle)),
        "expected an error containing {needle:?}, got: {:#?}",
        out.errors.iter().map(|e| &e.msg).collect::<Vec<_>>()
    );
}

#[test]
fn tour_example_checks() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/tour.tl")).unwrap();
    let out = check_source(&src);
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    assert!(out.warnings.is_empty(), "{:#?}", out.warnings);
    assert_eq!(out.vcs.iter().filter(|v| v.status == VcStatus::Pending).count(), 1);
}

// ---- nominal types ----

#[test]
fn nominal_is_not_its_underlying_type() {
    err("fn f(u: UserId) -> Int { u }", "expected `Int`, found `UserId`");
    err("fn f(x: Int) -> UserId { x }", "expected `UserId`, found `Int`");
    ok("fn f(u: UserId) -> Int { u.0 }");
}

#[test]
fn nominal_invariant_checked_at_construction() {
    err("fn f() -> Port { Port(70000) }", "refinement violated");
    ok("fn f() -> Port { Port(443) }");
}

// ---- records and tuples ----

#[test]
fn records_are_exact_by_default() {
    err("fn f(r: { a: Int }) -> Int { r.a }\nfn g() -> Int { f({ a: 1, b: 2 }) }", "type mismatch");
    ok("fn f(r: { a: Int, .. }) -> Int { r.a }\nfn g() -> Int { f({ a: 1, b: 2 }) }");
}

#[test]
fn tuples_are_positional_records() {
    ok("fn f(p: (Int, (Str, Bool))) -> Bool { p.1.1 }");
    ok("fn f(p: (Int, Str)) -> (Int, Str) { (p.0, p.1) }");
    err("fn f(p: (Int, Str)) -> Int { p.2 }", "no field `2`");
}

// ---- never ----

#[test]
fn uninhabited_types_collapse_to_never() {
    ok("type T = (Int, Never)\nfn f(x: T) -> Str { match x {} }");
    ok("type T = Int where false\nfn f(x: T) -> Str { match x {} }");
    ok("fn f() -> Int { panic(\"no\") }");
}

// ---- unions ----

#[test]
fn union_subsumption_normalizes() {
    // `Nat | Int` is just `Int`.
    err("fn f(x: Nat | Int) -> Str { x }", "found `Int`");
}

#[test]
fn match_narrows_and_checks_exhaustiveness() {
    ok("fn f(x: Int | Str) -> Int { match x { n: Int => n, s: Str => 0 } }");
    err("fn f(x: Int | Str) -> Int { match x { n: Int => n } }", "values of type `Str` are not covered");
    // A refined type test does not cover its base type.
    err("fn f(x: Int) -> Int { match x { n: Nat => n } }", "values of type `Int` are not covered");
    err("fn f(x: Int | Str) -> Int { match x { b: Bool => 0, _ => 1 } }", "can never have type `Bool`");
}

#[test]
fn unreachable_arm_warns() {
    let out = ok("fn f(x: Int) -> Int { match x { n => n, m: Int => m } }");
    assert!(out.warnings.iter().any(|w| w.msg.contains("unreachable")));
}

#[test]
fn field_access_on_unions_requires_every_member() {
    ok("fn f(x: { a: Int, b: Str } | { a: Int }) -> Int { x.a }");
    err("fn f(x: { a: Int, b: Str } | { a: Int }) -> Str { x.b }", "not every member");
}

// ---- disjoint unions ----

#[test]
fn enum_exhaustiveness() {
    err(
        "fn f(s: Shape) -> Int { match s { Shape.Circle { r } => 1, Shape.Rect(w, h) => 2 } }",
        "`Shape.Empty` are not covered",
    );
    ok("fn f(s: Shape.Circle | Shape.Rect) -> Int { match s { Shape.Circle { .. } => 1, Shape.Rect(_, _) => 2 } }");
}

#[test]
fn variants_are_subtypes_of_their_enum() {
    ok("fn f() -> Shape { Shape.Rect(1.0, 2.0) }");
    err("fn f() -> Shape.Circle { Shape.Empty }", "type mismatch");
    err("fn f(s: Shape.Rect) -> Int { match s { Shape.Empty => 0, _ => 1 } }", "can never match");
}

#[test]
fn variant_payload_refinements_are_checked() {
    err("fn f() -> Shape { Shape.Circle { r: 0.0 } }", "refinement violated");
}

// ---- refinements ----

#[test]
fn call_site_obligations() {
    err("fn f() -> Int { div(1, 0) }", "refinement violated");
    err("fn f() -> Int { let z = 3; div(1, z - 3) }", "refinement violated");
    ok("fn f(b: Int) -> Int { if b != 0 { div(1, b) } else { 0 } }");
}

#[test]
fn dependent_parameters() {
    let src = "fn clamp(lo: Int, hi: Int where hi >= lo, x: Int) -> Int { x }";
    err(&format!("{src}\nfn f() -> Int {{ clamp(5, 1, 3) }}"), "refinement violated");
    ok(&format!("{src}\nfn f() -> Int {{ clamp(1, 5, 3) }}"));
}

#[test]
fn unprovable_obligations_are_pending() {
    let out = ok("fn f(x: Int) -> Nat { x }");
    assert_eq!(out.vcs.len(), 1);
    assert_eq!(out.vcs[0].status, VcStatus::Pending);
}

#[test]
fn predicate_fragment_is_enforced() {
    err("type T = Int where self * self > 4", "non-linear");
    err("type T = Int where div(self, 2) > 4", "not allowed in a refinement predicate");
    err("type T = Int where self", "must be `Bool`");
}

// ---- misc ----

#[test]
fn recursive_alias_is_rejected_but_recursive_enum_is_fine() {
    err("type L = (Int, L)", "recursive");
    ok("enum List { Nil, Cons(Int, List) }\nfn len(l: List) -> Int { match l { List.Nil => 0, List.Cons(_, t) => 1 + len(t) } }");
}

#[test]
fn shadowing_is_rejected() {
    err("fn f(x: Int) -> Int { let x = 1; x }", "shadowing");
}
