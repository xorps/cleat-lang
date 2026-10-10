# test-lang

A small language with nominal types, tuples, `Never`, anonymous records,
untagged unions, disjoint unions (enums) and refinement types.
See [SPEC.md](SPEC.md) for the design.

## Prototype

```sh
cargo run -- check examples/tour.tl --vcs   # type-check and list verification conditions
cargo test
```

The prototype implements parsing, base type checking with subtyping and
union normalization, narrowing and exhaustiveness for `match`, and
verification-condition generation for refinements. VCs are discharged only
by constant folding and syntactic matching; the rest are reported as
`pending` until the SMT backend lands.

| Module | Role |
|---|---|
| `src/lexer.rs`, `src/parser.rs` | Surface syntax → AST |
| `src/types.rs` | `Ty`, normalization (§4), subtyping (§3), narrowing |
| `src/check.rs` | Bidirectional checker, patterns, VC generation (§5–6) |
| `src/vc.rs` | VC representation and trivial discharge |

Not yet implemented: SMT (Z3) backend, generics, measures, mutable fields,
modules, `--gradual`, code generation. Variable shadowing is rejected for now
because refinement facts refer to variables by name.
