# test-lang

A typed configuration language whose values are exactly JSON, aimed at
replacing Helm and Kustomize for Kubernetes. It has structural,
TypeScript-style types (literal unions, optional fields, defaults,
refinements), typed inputs, and errors that name the exact field and
constraint that failed. See [SPEC.md](SPEC.md).

```sh
cargo build
./target/debug/tl check examples/webapp.tl
./target/debug/tl export examples/webapp.tl --set env=prod --set image=ghcr.io/acme/api:1.4.2
./target/debug/tl export examples/webapp.tl --set env=prod --set image=api:1 --target argocd
./target/debug/tl export examples/webapp.tl --set env=prod --set image=api:1 \
    --target flux --out deploy --flux-source infra --flux-path deploy
cargo test
```

## What fails, and when

| Mistake | Caught by | Message |
|---|---|---|
| `metdata:` typo | `tl check` | ``unknown field `metdata` in `Svc`; did you mean `metadata`?`` |
| `port: 70000` literal | `tl check` | ``refinement violated: `1 <= 70000 && 70000 <= 65535` is false (required by `Port`)`` |
| optional field used without a check | `tl check` | `` `m.labels` may be null; use `?.` or `??` `` |
| `--set env=production` | `tl export` | ``input `env`: expected `Env` (`"dev" \| "staging" \| "prod"`), got "production"`` |
| computed port out of range | `tl export` | ``argument `port` of `container`: 70080 does not satisfy …``, followed by the call stack |
| component dependency cycle | `tl check` | `dependency cycle between components: a -> b -> a` |

## Layout

| Module | Role |
|---|---|
| `src/lexer.rs`, `src/parser.rs` | Source to AST (dotted keys, comprehensions, interpolation) |
| `src/types.rs` | Types, normalization, subtyping, narrowing |
| `src/check.rs` | Static checker: bidirectional, field-aware diagnostics, VCs |
| `src/vc.rs` | Refinement obligations: proved, refuted, or left to runtime |
| `src/eval.rs` | Evaluator, input decoding, runtime type checks with defaults |
| `src/emit.rs`, `src/json.rs` | YAML/JSON output with safe quoting; JSON input parsing |
| `src/targets.rs` | `plain`, `argocd` (sync waves), `flux` (`dependsOn`) |

Next up: Kubernetes OpenAPI import, so `Deployment`, `Service` and CRDs come
with real types instead of hand-written ones.
