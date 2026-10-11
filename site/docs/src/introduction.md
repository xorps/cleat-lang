<div class="cleat-hero">
  <img class="light-only" src="assets/logo.svg" alt="Cleat">
  <img class="dark-only" src="assets/logo-dark.svg" alt="Cleat">
  <p class="tagline">Typed configuration for Kubernetes.</p>
  <div class="cleat-buttons">
    <a class="primary" href="playground/">Try it in your browser</a>
    <a href="getting-started.html">Get started</a>
    <a href="https://github.com/xorps/cleat-lang">GitHub</a>
  </div>
</div>

Cleat is a configuration language whose values are JSON and whose types come
from the real Kubernetes API and your CRDs. Mistakes are caught while you
write, at the exact field that's wrong, and the output is plain YAML for
Argo CD, Flux, or `kubectl apply`.

> **Cleat is under active development.** The language, the CLI and the editor
> and Argo CD integrations can change in incompatible ways, and it hasn't
> been used in production yet. "Cleat" is a working name.

## A taste

```ts
import * as k8s from "../schemas/kubernetes-1.37.json"

input env: "dev" | "prod"
input image: Str
input replicas: Int where self >= 1 = 2

export k8s.Deployment {
  metadata.name: "api",
  spec: {
    replicas: if env == "prod" then max(replicas, 3) else replicas,
    selector.matchLabels.app: "api",
    template: {
      metadata.labels.app: "api",
      spec.containers: [{ name: "api", image, ports: [{ containerPort: 8080 }] }],
    },
  },
}
```

`apiVersion` and `kind` fill themselves in. Misspell `replicas`, or write
`imagePullPolicy: "Allways"`, and the mistake is reported before anything
reaches a cluster:

```console
$ cleat check examples/quickstart.cleat
quickstart.cleat:13:5: error: unknown field `replcias` in `k8s.apps.v1.DeploymentSpec`; did you mean `replicas`?
quickstart.cleat:17:64: error: type mismatch: expected `"Always" | "IfNotPresent" | "Never"`, found `"Allways"`
```

Bad inputs are rejected before evaluation, all of them at once:

```console
$ cleat export examples/quickstart.cleat --set env=production --set image=api:1 --set replicas=0
quickstart.cleat:6:1: error: input `env`: expected `"dev" | "prod"`, got "production" (string)
quickstart.cleat:8:1: error: input `replicas`: 0 does not satisfy `self >= 1`
```

## Why Cleat

- **Types from the real API.** Import the Kubernetes OpenAPI spec and your
  CRD files; every resource, field, required field and enum is checked, and
  field documentation shows on hover.
- **Typed inputs instead of `values.yaml`.** Inputs declare their types,
  defaults and constraints, and bad values are rejected up front.
- **Overlays that can't silently miss.** Patches are checked against the
  types; a misspelled field or a selector that matches nothing is an error.
- **Dependencies between components** become Argo CD sync waves or Flux
  `dependsOn`.
- **Small and familiar.** TypeScript-style structural types, ES-style
  imports, and no recursion or I/O, so evaluation always terminates and
  depends only on the repository.
- **Made for editors.** Completion with Kubernetes docs, hover,
  go to definition, errors as you type, and a live YAML preview.

## Compared with

| | Helm | Kustomize | Cleat |
|---|---|---|---|
| How you write config | Go templates over YAML text | YAML plus patches | A typed language that outputs YAML |
| Checks against the Kubernetes API | After rendering | After rendering | While writing, with the field and a fix |
| Inputs | `values.yaml` (optional JSON Schema) | none | Typed `input`s with defaults and constraints |
| Variations per environment | values files | overlays | modules and typed `with` patches |
| Ordering between resources | hooks and weights | by kind only | `after` → sync waves / `dependsOn` |
