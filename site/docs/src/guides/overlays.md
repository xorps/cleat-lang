# Bases and environment overlays

Cleat replaces Kustomize bases and overlays with modules and typed patches.
A module holds the shared pieces; each environment imports it and patches
the result.

## A shared base

A module is a `.cleat` file that defines types, functions and constants. It
can import schemas and refine types, but it can't declare `input`s or
`export` anything: it takes values as function parameters instead.

```ts
// examples/overlays/base.cleat
import * as k8s from "../../schemas/kubernetes-1.37.json"

// Project policy: no NodePort or ExternalName Services.
refine k8s.ServiceSpec with { type: "ClusterIP" | "LoadBalancer" }

type DnsLabel = Str where len(self) <= 63 && matches(self, "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$")
type Port     = Int where 1 <= self && self <= 65535

fn labels(app: Str) -> { [Str]: Str } = {
  "app.kubernetes.io/name": app,
  "app.kubernetes.io/part-of": "shop",
}

/// A Deployment and a Service for one HTTP app.
fn app(name: DnsLabel, image: Str, port: Port = 8080) -> [k8s.Deployment | k8s.Service] = [
  k8s.Deployment {
    metadata: { name, labels: labels(name) },
    spec: {
      replicas: 1,
      selector.matchLabels: labels(name),
      template: {
        metadata.labels: labels(name),
        spec.containers: [{ name, image, ports: [{ name: "http", containerPort: port }] }],
      },
    },
  },
  k8s.Service {
    metadata: { name, labels: labels(name) },
    spec: { selector: labels(name), ports: [{ name: "http", port: 80, targetPort: "http" }] },
  },
]
```

## An overlay

```ts
// examples/overlays/prod.cleat
import * as base from "./base.cleat"

input image: Str

export api = base.app("api", image) with {
  [kind: "Deployment"].spec.replicas: 3,
  [kind: "Deployment"].metadata.labels +: { tier: "prod" },
  [kind: "Deployment"].spec.template.spec.containers[name: "api"].resources.limits: { cpu: "1", memory: "512Mi" },
  [kind: "Deployment"].spec.template.spec.containers[name: "api"].env +: [{ name: "LOG_LEVEL", value: "warn" }],
  [kind: "Service"].spec.type: "LoadBalancer",
}
```

| Entry | Effect |
|---|---|
| `path: v` | Set the value at `path`, creating missing objects on the way. |
| `path +: v` | Merge: lists append; objects follow JSON Merge Patch (nested objects merge, `null` removes a key). |
| `-path` | Remove a field, or the selected list elements. |

| Path segment | Selects |
|---|---|
| `.f` | a field |
| `[]` | every list element or map value |
| `[2]` | one list element |
| `[name: "api", ...]` | the list elements whose fields equal these values |

## Mistakes are errors, not no-ops

Patches into declared types (like `k8s.Deployment`) are checked against
them, and a selector that matches nothing is an error. The base's `refine`
applies to the overlay too:

```ts
import * as base from "./base.cleat"
input image: Str
export api = base.app("api", image) with {
  [kind: "Deployment"].spec.replcias: 3,
  [kind: "Service"].spec.type: "NodePort",
}
```

```console
$ cleat check prod.cleat
prod.cleat:4:3: error: `[kind: "Deployment"].spec.replcias`: `base.k8s.apps.v1.DeploymentSpec` has no field `replcias`; did you mean `replicas`?
prod.cleat:5:32: error: type mismatch: expected `"ClusterIP" | "LoadBalancer"`, found `"NodePort"`
2 error(s), 0 warning(s); refinements: 1 proved statically, 0 checked at runtime
```

In anonymous objects (no declared type), a patch updates the type
structurally instead: `{ n: 1 } with { n: "one" }` has type `{ n: "one" }`.
