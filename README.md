<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <img alt="Cleat" src="assets/logo.svg" width="440">
  </picture>
</p>

> [!WARNING]
> **Cleat is under active development.** The language, the CLI and the
> editor and Argo CD integrations can change in incompatible ways between
> commits, and it hasn't been used in production yet.

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
fill themselves in, descriptions show on hover, and enum fields the spec
leaves as plain strings (`Service.spec.type`, `imagePullPolicy`,
`restartPolicy`, protocols, access modes, ...) are checked against their
allowed values:

```ts
import * as k8s from "../schemas/kubernetes-1.37.json"
import * as cm from "./crds/cert-manager.crds.yaml"

export k8s.Deployment { metadata.name: "web", spec: { replicas: 2, ... } }
export cm.Certificate { metadata.name: "web-tls", spec: { secretName: "web-tls", ... } }
```

Download the schema the example uses (or another version) with:

```sh
schemas/fetch.sh 1.37
```

`cleat gen <schema>` prints the generated Cleat types. Imports follow
ECMAScript: `import * as ns`, `import { A, B }`, and data files with
`import conf from "./nginx.conf" with { type: "text" }` (or `"json"`/`"yaml"`).

## Shared bases and environment overlays

A module holds the shared pieces; each environment imports it and patches
the result with `with` (the role of Kustomize overlays):

```ts
// examples/overlays/prod.cleat
import * as base from "./base.cleat"
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
- Imports can't read outside the repository checkout (`..` and symlinks
  included), so a repository can't use the plugin to read the sidecar's files.

`deploy/argocd/e2e/run.sh` runs all of this against a throwaway kind cluster
(Argo CD stable, an in-cluster git server, three dependent components) and
checks parameters, patches, sync waves and their order, and error reporting.

## Editor support

`cleat lsp` is a Language Server (stdio) that works with any LSP client:

- diagnostics as you type;
- completion of object keys (from the expected type, including Kubernetes and
  CRD types, with their docs; keys already set are left out, required ones
  come first), field names after `.`, members of imported modules, `with`
  patch paths, and values (enum literals, variables and functions that fit);
- completion of type names (including `k8s.` namespaces), type-level `with`
  paths, and `match` arms (literals, and `{ kind: "Service", .. }` shapes for
  discriminated unions);
- hover for types, fields, functions and bindings, with OpenAPI descriptions;
- go to definition for variables (including parameters, loop and pattern
  variables), functions, types (Kubernetes and CRD types open the generated
  type source), imported module members and import paths;
- unsaved edits to imported files are seen immediately, and every open file
  is re-checked when something it imports changes;
- with a `<file>.inputs.json` (or `.yaml`) next to the file, errors that only
  show up for those inputs are reported inline too.

For VS Code (no npm needed):

```sh
cargo build
editors/vscode/package.sh
code --install-extension editors/vscode/cleat.vsix --force
```

The extension uses the newest of `target/debug/cleat` and
`target/release/cleat` in the open workspace, or `cleat.serverPath`.
**Cleat: Show Output** (the preview icon in the editor title bar) opens the
exported YAML beside the file and updates as you type.
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

## CI and releases

The Rust version (currently 1.99.0) is pinned in `rust-toolchain.toml`, which
`rustup` picks up locally, in CI and in the release and Docker builds.

`.github/workflows/ci.yml` runs on every push: `cargo fmt --check`, clippy
with warnings as errors, the tests (Linux and macOS, with the Kubernetes
schema fetched), the examples, the VS Code extension package, and a smoke
test of the Argo CD sidecar image. The Argo CD end-to-end test runs weekly
and on demand.

Pushing a tag like `v0.1.0` runs `release.yml`: binaries for Linux and macOS
(x86_64 and arm64), the `.vsix`, and a multi-arch sidecar image at
`ghcr.io/<owner>/cleat-cmp:<tag>`, attached to a GitHub release.

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
