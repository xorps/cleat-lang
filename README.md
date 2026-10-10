# Cleat

A typed configuration language whose values are exactly JSON, aimed at
replacing Helm and Kustomize for Kubernetes. It has structural,
TypeScript-style types (literal unions, optional fields, defaults,
refinements), typed inputs, and errors that name the exact field and
constraint that failed. See [SPEC.md](SPEC.md).

```sh
cargo build
./target/debug/cleat check examples/webapp.cleat
./target/debug/cleat export examples/webapp.cleat --set env=prod --set image=ghcr.io/acme/api:1.4.2
./target/debug/cleat export examples/webapp.cleat --set env=prod --set image=api:1 --target argocd
./target/debug/cleat export examples/webapp.cleat --set env=prod --set image=api:1 \
    --target flux --out deploy --flux-source infra --flux-path deploy
cargo test
```

## Kubernetes and CRD types

Import the Kubernetes OpenAPI spec and your CRDs; `apiVersion` and `kind`
fill themselves in, and descriptions show on hover:

```ts
import k8s from "../schemas/kubernetes-1.31.json"
import cm from "./crds/cert-manager.crds.yaml"

export k8s.Deployment { metadata.name: "web", spec: { replicas: 2, ... } }
export cm.Certificate { metadata.name: "web-tls", spec: { secretName: "web-tls", ... } }
```

Download the schema the example uses (or another version) with:

```sh
schemas/fetch.sh 1.31
```

`cleat gen <schema>` prints the generated Cleat types.

## Shared bases and environment overlays

A module holds the shared pieces; each environment imports it and patches
the result with `with` (the role of Kustomize overlays):

```ts
// examples/overlays/prod.cleat
import base from "./base.cleat"
input image: Str

export api = base.app("api", image) with {
  [kind: "Deployment"].spec.replicas: 3,
  [kind: "Deployment"].metadata.labels +: { tier: "prod" },
  [kind: "Service"].spec.type: "LoadBalancer",
}
```

Patches are checked against the Kubernetes types: a misspelled field, a
value of the wrong type, or a selector that can't match is an error.

## Argo CD

Cleat runs as an Argo CD [config management plugin](https://argo-cd.readthedocs.io/en/stable/operator-manual/config-management-plugins/) sidecar:

```sh
docker build -f deploy/argocd/Dockerfile -t <registry>/cleat-cmp:<tag> .   # then push it
# set the image in deploy/argocd/repo-server-patch.yaml, then:
kubectl -n argocd patch deployment argocd-repo-server --patch-file deploy/argocd/repo-server-patch.yaml
```

```yaml
# Application
spec:
  source:
    repoURL: https://github.com/acme/deploy.git
    path: apps/shop
    plugin:
      name: cleat-v1
      env:                                  # optional
        - { name: CLEAT_FILE, value: prod.cleat }   # default: main.cleat
      parameters:
        - { name: image, string: ghcr.io/acme/api:1.4.2 }
```

- Each `input` is announced to the Argo CD UI as a parameter: required if it
  has no default, with its type (e.g. `Env = "dev" | "prod"`) as the tooltip.
- Parameters are decoded by type and validated before evaluation; invalid
  ones show up in the Application's conditions with Cleat's message.
- Output uses the `argocd` target, so `export x after y` becomes sync waves
  (`CLEAT_TARGET=plain` turns this off).

`deploy/argocd/e2e/run.sh` runs all of this against a throwaway kind cluster
(Argo CD stable, an in-cluster git server, three dependent components) and
checks parameters, patches, sync waves and their order, and error reporting.

## Editor support

`cleat lsp` is a Language Server (stdio) with diagnostics as you type and
hover for types, fields and functions; it works with any LSP client.

For VS Code (no npm needed):

```sh
cargo build
editors/vscode/package.sh
code --install-extension editors/vscode/cleat.vsix --force
```

The extension uses `target/debug/cleat` from the open workspace, or
`cleat.serverPath`. **Cleat: Show Output** (the preview icon in the editor
title bar) opens the exported YAML beside the file and refreshes on save.
Inputs come from `<file>.inputs.json` next to the file and the
`cleat.exportArgs` setting.

## What fails, and when

| Mistake | Caught by | Message |
|---|---|---|
| `metdata:` typo | `cleat check` | ``unknown field `metdata` in `Svc`; did you mean `metadata`?`` |
| `port: 70000` literal | `cleat check` | ``refinement violated: `1 <= 70000 && 70000 <= 65535` is false (required by `Port`)`` |
| optional field used without a check | `cleat check` | `` `m.labels` may be null; use `?.` or `??` `` |
| `--set env=production` | `cleat export` | ``input `env`: expected `Env` (`"dev" \| "staging" \| "prod"`), got "production"`` |
| computed port out of range | `cleat export` | ``argument `port` of `container`: 70080 does not satisfy …``, followed by the call stack |
| component dependency cycle | `cleat check` | `dependency cycle between components: a -> b -> a` |

## Layout

| Module | Role |
|---|---|
| `src/lexer.rs`, `src/parser.rs` | Source to AST (dotted keys, comprehensions, interpolation) |
| `src/types.rs` | Types, normalization, subtyping, narrowing |
| `src/check.rs` | Static checker: bidirectional, field-aware diagnostics, VCs |
| `src/vc.rs` | Refinement obligations: proved, refuted, or left to runtime |
| `src/eval.rs` | Evaluator, input decoding, runtime type checks with defaults |
| `src/emit.rs`, `src/json.rs` | YAML/JSON output with safe quoting; JSON input parsing |
| `src/openapi.rs`, `src/modules.rs` | Schema-to-type generation; import loading and caching |
| `src/targets.rs` | `plain`, `argocd` (sync waves), `flux` (`dependsOn`) |
| `src/lsp.rs` | Language server: diagnostics and hover |
| `editors/vscode/` | VS Code extension: highlighting, LSP client, output preview |

Next up: Kubernetes OpenAPI import, so `Deployment`, `Service` and CRDs come
with real types instead of hand-written ones.
