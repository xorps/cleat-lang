# A tour of the language

Every value in Cleat is a JSON value: `null`, booleans, numbers, strings,
lists and objects. Types describe those values, structurally, the way
TypeScript does. You can run any of this in the [playground](playground/).

## Types

```ts
type Env = "dev" | "staging" | "prod"            // a union of literal types
type Port = Int where 1 <= self && self <= 65535 // a refinement

type Container = {
  name: Str,                                      // required
  image: Str,
  port?: Port,                                    // optional
  pullPolicy: "Always" | "IfNotPresent" = "IfNotPresent",  // with a default
}

type Labels = { [Str]: Str }                      // a map
type Endpoint = (Str, Port)                       // a tuple
```

The base types are `Bool`, `Int`, `Float`, `Str`, `null`, `Never` (no
values) and `Json` (any value). Records are exact unless they end with `..`,
which allows other fields. Keys may be quoted:
`"app.kubernetes.io/name": Str`.

## Inputs

Inputs are the values supplied at export time, from `--set`, an inputs file
or Argo CD parameters. Each one has a type, and optionally a default:

```ts
input env: Env
input image: Str
input replicas: Int where self >= 1 = 2
input debug: Bool = false
```

All inputs are decoded and checked before evaluation starts, and every bad
input is reported at once.

## Constants and functions

```ts
let app = "api"

fn labels(name: Str, tier: Str = "backend") -> Labels = {
  "app.kubernetes.io/name": name,
  tier,                                  // shorthand for `tier: tier`
}

fn url(e: Endpoint) -> Str = "http://\(e.0):\(e.1)"
```

A function's body is one expression. Calls take positional or named
arguments: `labels(app, tier: "web")`. A function can only call functions
declared above it, so there is no recursion and evaluation always ends.

## Choices

```ts
fn size(e: Env) -> Int = match e {
  "prod" => 3,
  "staging" => 2,
  "dev" => 1,
}
```

`match` over a union of literals is checked for exhaustiveness: leave out
`"staging"` and it's an error. `if c then a else b` is an expression.
`a ?? b` gives `b` when `a` is `null`, and `x?.f` reads a field of a value
that may be `null`. Comparing with `null` narrows the type, as in
TypeScript.

## Objects and lists

```ts
let main: Container = { name: app, image, port: 8080 }
let sidecar: Container = { name: "log", image: "busybox:1.37" }

let containers = [main, if debug then sidecar]
let ports = [for c in containers => if c.port != null then c.port]
let names = { for c in containers => { [c.name]: c.image } }
```

- `let x: T = e` checks `e` against `T` and fills in defaults (here
  `pullPolicy`).
- `if` without `else` inside a list or object adds the element only when
  the condition holds.
- `for x in xs => e` is a comprehension. In an object, it computes keys.
- Dotted keys nest objects: `metadata.labels.app: "api"` is
  `metadata: { labels: { app: "api" } }`.
- `...e` spreads a list or an object.

## Strings

Interpolation is `\(expr)`, so `${VAR}` in shell commands is left alone.
Multi-line strings start after `"""` and end at a `"""` on its own line,
whose indentation is removed from every line:

```ts
let conf = """
    server {
      listen \(main.port ?? 80);
    }
    """
```

## Output

Each `export` emits documents (a list emits one document per element):

```ts
export {
  name: app,
  labels: labels(app, tier: "web"),
  replicas: if env == "prod" then max(replicas, size(env)) else replicas,
  containers,
  conf,
}
```

```console
$ cleat export tour.cleat --set env=prod --set image=ghcr.io/acme/api:1.4.2
name: api
labels:
  app.kubernetes.io/name: api
  tier: web
replicas: 3
containers:
  - name: api
    image: ghcr.io/acme/api:1.4.2
    port: 8080
    pullPolicy: IfNotPresent
conf: |-
  server {
    listen 8080;
  }
```

## Imports

```ts
import * as k8s from "../schemas/kubernetes-1.37.json"          // types from a schema
import * as base from "./base.cleat"                             // a Cleat module
import { Deployment, Service } from "../schemas/kubernetes-1.37.json"
import defaults from "./defaults.yaml" with { type: "yaml" }      // data, typed from its contents
import nginxConf from "./nginx.conf" with { type: "text" }
```

Import paths are string literals, all imports are resolved before
evaluation, and they can't leave the project root (the nearest directory
with `.git`, or `--root`).

## Patches

`e with { ... }` returns a copy of `e` with changes, checked against the
value's type:

```ts
export base.app("api", image) with {
  [kind: "Deployment"].spec.replicas: 3,
  [kind: "Deployment"].metadata.labels +: { tier: "prod" },
  -[kind: "Deployment"].spec.strategy,
}
```

See [Bases and environment overlays](guides/overlays.md).

## Built-in functions

`len`, `keys`, `values`, `str`, `upper`, `lower`, `join`, `split`,
`contains`, `starts_with`, `ends_with`, `replace`, `matches` (regex), `min`,
`max`, `sort`, `to_json`, `to_yaml`, `b64encode` and `error`. Hover over one
in an editor to see its signature.
