# Cleat Language Specification (draft v0.2)

A typed configuration language whose values are exactly JSON. It is aimed
at Kubernetes as a replacement for Helm and Kustomize, with TypeScript-style
structural types and error messages that point at the exact field and
constraint that failed.

Decisions marked **(provisional)** unblock work and are expected to be
revisited.

## 1. Principles

- **Values are JSON.** Every value is `null`, a boolean, a number, a string, a
  list, or an object. Nothing else can reach the output.
- **Types are structural.** There are no nominal types and no identity. Two
  types with the same shape are the same type.
- **Exact by default.** An object type lists every field it allows, so
  `replcias: 3` is an error. `..` opts into extra fields.
- **Check early, fail precisely.** `cleat check` proves what it can without
  inputs. `cleat export` decodes inputs against their declared types and checks
  every annotated value during evaluation. An error names the source location,
  the value's path (`spec.template.spec.containers[0].ports[0]`), and the
  constraint that failed.
- **Low ceremony.** Dotted keys, defaults, fields filled in from single-value
  types, conditional fields and comprehensions keep files close to the YAML
  they produce.
- **Evaluation terminates (provisional).** No general recursion: a function
  can only call functions declared above it.

## 2. Values

```
null | true | false | <int> | <float> | "<string>" | [v, ...] | { key: v, ... }
```

- Ints are 64-bit and floats are IEEE doubles. Both serialize as JSON numbers.
- Objects keep the order of their keys. When a value is checked against a
  record type, fields filled in from a single-literal type (`apiVersion`,
  `kind`) come first, then the given fields in source order, then other
  defaults.
- Tuples are lists of fixed length.

## 3. Types

```
T ::= Never | null | Bool | Int | Float | Str | Json
    | "lit" | 42 | 1.5 | true | false     -- literal (singleton) types
    | [T]                                 -- list
    | (T1, T2, ...)                       -- tuple (a fixed-length list)
    | { [Str]: T }                        -- map: any keys, values of type T
    | { f: T, g?: T, h: T = e, "a/b": T, .. }   -- record
    | T1 | T2                             -- union
    | T where φ                           -- refinement (binder: `self`)
    | Name                                -- type alias
```

- `Json` is the type of every value:
  `null | Bool | Float | Str | [Json] | { [Str]: Json }`.
- `Int <: Float`, and a literal is a subtype of its base type (`"dev" <: Str`).
- In a record:
  - `f: T` is required.
  - `g?: T` is optional. Reading it gives `T | null`, and a `null` or missing
    value is left out of the output.
  - `h: T = e` has a default. Callers may omit it; when a value is checked
    against the record, the default is filled in. Reading it gives `T`.
  - A field whose type is a single literal (`kind: "Deployment"`) has that
    literal as an implicit default.
  - `..` makes the record **open**: any other key is allowed, with a `Json`
    value.
  - Keys may be quoted: `"app.kubernetes.io/name": Str`.
- Type aliases may not be recursive. Use `Json` for recursive data.
  **(provisional)**

### 3.1 Subtyping

| Rule | Statement |
|---|---|
| Bottom/top | `Never <: T <: Json` for every `T` |
| Numbers | `Int <: Float`; a literal is a subtype of its base type |
| Unions | `A \| B <: C` iff both are; `A <: B \| C` if either holds |
| Lists | covariant |
| Maps | covariant; a record is a map if every field fits the value type (and, if open, the value type is `Json`) |
| Records | for each field `f` of the target: a required field must be present and required in the source; an optional or defaulted field may be absent. The source may have extra fields only if the target is open. Fields are covariant. |
| Refinement | `T where φ <: S where ψ` needs `T <: S` and `φ ⇒ ψ` (§6) |

### 3.2 Normalization

Unions are flattened, `Never` members are dropped, and any member that is a
subtype of another member is removed (`"dev" | Str` becomes `Str`). Records
with a required `Never` field, and `T where false`, become `Never`.
`true | false` is `Bool`.

## 4. Programs

A file is a sequence of top-level items. Everything except type aliases and
imports must be declared before it is used.

```ts
type Env = "dev" | "staging" | "prod"

input env: Env                          // supplied at export time
input replicas: Int where self >= 1 = 2 // with a default

let name = "api"                        // top-level constant

fn port_name(p: Int) -> Str = "http-\(p)"   // functions; the body is one expression

export { kind: "ConfigMap", ... }       // each `export` emits documents
```

- `input name: T (= default)?` declares a value supplied from outside (§8).
- `let name (: T)? = e` binds a constant.
- `fn name(params) (-> T)? = e` declares a function. A parameter may have a
  default (`port: Int = 8080`). Calls may use positional or named arguments
  (`f(1, port: 80)`).
- `export e` emits `e` as output. If `e` is a list, each element is a separate
  document. `export name (after a, b)? = e` exports a named **component** (§9).
- Shadowing is allowed. Facts the checker knows about the outer variable are
  dropped while the inner one is in scope.

### 4.1 Imports

```ts
import k8s from "../schemas/kubernetes-1.31.json"    // OpenAPI (Swagger 2 or OpenAPI 3)
import cm from "./crds/cert-manager.crds.yaml"         // CustomResourceDefinitions
import lib from "./types.cleat"                        // `type` declarations

let d = k8s.Deployment { ... }
fn issuer() -> cm.ClusterIssuer = ...
```

An import brings a module's types in under a namespace: `ns.T`. Paths are
relative to the importing file. Imported modules may only declare types for
now (no functions, `let`s or nested imports).

Schemas are converted to Cleat types (`cleat gen <file>` prints the result):

| Schema | Cleat type |
|---|---|
| `$ref` | the named type; a reference that closes a cycle becomes `Json` |
| `type: object` with `properties` | exact record; fields not in `required` are optional (`?`) |
| `additionalProperties: true`, `x-kubernetes-preserve-unknown-fields` | open record (`..`) |
| `type: object` with `additionalProperties: T` | `{ [Str]: T }` |
| `type: array` | `[T]` |
| `enum` | union of literal types |
| `x-kubernetes-int-or-string`, `format: int-or-string` | `Int \| Str` |
| `pattern`, `minLength`, `maxLength` | `Str where ...` (patterns Rust's regex engine rejects are dropped) |
| `minimum`, `maximum`, `exclusive*` | `Int where ...` / `Float where ...` |
| `nullable: true` | `T \| null` |
| `x-kubernetes-group-version-kind` | `apiVersion`/`kind` become required literal fields, so they fill themselves in |
| Kubernetes `resource.Quantity` | `Str \| Float` (accepts `cpu: 1` and `cpu: "500m"`) |

**Names.** OpenAPI definitions keep their group and version:
`io.k8s.api.apps.v1.Deployment` becomes `apps.v1.Deployment`. A short name
(`Deployment`) is added when one candidate is clearly preferred: GA over beta
over alpha, then the higher version. When candidates tie (`core.v1.Event`
and `events.v1.Event`), there is no short name, and using it is an error
that lists the qualified names. CRD kinds are named `<version>.<Kind>`, with
nested objects named after their path (`v1.CertificateSpec`). The short
`Kind` points to the storage version. OpenAPI descriptions are shown on
hover.

### 4.2 Refining types: `with` and `refine`

Imported schemas are often looser than you want: the Kubernetes OpenAPI
spec, for instance, types `Service.spec.type` as any `Str`. `T with { ... }`
makes a narrower copy of a record type:

```ts
type SmallDeployment = k8s.Deployment with {
  spec.replicas where self <= 10,                                    // add a predicate
  spec.template.spec.containers[].image where contains(self, ":"),   // `[]`: each list element / map value
  spec.strategy.type: "RollingUpdate" | "Recreate",                  // replace the type
  spec.revisionHistoryLimit: Int = 3,                                // replace the type and set a default
  -status,                                                           // remove an optional field
}
```

Paths go through nested records, optional (`T | null`) values, and named
types. **`with` only narrows:** a replacement type must be a subtype of the
original, and only optional fields can be removed. So the result is always
a subtype of the original type, and a value that satisfies your refined type
also satisfies the upstream schema. Required/optional status of a field is
kept. On an open record, `f: T` may add a field that isn't declared.

`refine T with { ... }` narrows a type **in place**: every reference to `T`
in the file, including those inside other imported types, sees the
narrowed version.

```ts
refine k8s.ServiceSpec with { type: "ClusterIP" | "NodePort" | "LoadBalancer" | "ExternalName" }
let s = k8s.Service { spec.type: "Cluster" }   // error: expected one of the four
```

`refine` on a short name refines the definition it points to
(`k8s.core.v1.ServiceSpec`). Several `refine`s of the same type stack in
source order. Static checks and runtime checks both use the refined type.

## 5. Expressions

```ts
null  true  42  1.5  "text \(expr) more"  [a, b]  (a, b)  { k: v }
x   x.f   x.0   xs[i]   m["key"]   f(a, b, name: c)   T { ... }
-x  !x  a + b  a - b  a * b  a / b  a % b  a == b  a != b  a < b  ...
a && b   a || b   a ?? b   if c then a else b   let x = e; body
match e { pattern => e, ... }
```

- **Interpolation** is `\(expr)`, so shell `${VAR}` in container commands is
  left alone. Interpolated values must be `Str`, `Int`, `Float` or `Bool`.
- **`a ?? b`** gives `b` when `a` is `null`.
- **`T { ... }`** checks the object against `T` and fills in its defaults. It
  is not a constructor: the result is an ordinary value.
- **`xs[i]`** on a list has type `T` and fails at runtime when `i` is out of
  bounds. **`m["k"]`** on a map has type `T | null`.

### 5.1 Object literals

```ts
{
  name: "web",                         // field
  "app.kubernetes.io/name": "web",     // quoted key
  metadata.labels.app: "web",          // dotted key: nests objects
  ...base,                             // spread
  if env == "prod" then { replicas: 3 },        // conditional fields
  for k, v in extra => { [k]: v },     // computed keys from a comprehension
}
```

Members are applied in order. When a key is set twice and both values are
objects, they are merged deeply. Otherwise a duplicate key is an error.

### 5.2 List literals

```ts
[ a, ...rest, if debug then sidecar, for c in containers => c.name ]
```

The body of a `for` may be an `if` without `else`, which filters:
`for c in cs => if c.enabled then c`.

### 5.3 Patterns

Same as v0.1: `_`, `x`, `x: T` (type test), literals, `(p, q)`, `{ f: p, .. }`.
Literal patterns cover their literal type, so a `match` over
`"dev" | "staging" | "prod"`, or over `true` and `false`, can be exhaustive
without `_`. Type tests narrow unions, which makes discriminated unions on a
`kind` field work as in TypeScript:

```ts
match r {
  d: { kind: "Deployment", .. } => d.spec.replicas ?? 1,
  _ => 0,
}
```

### 5.4 Built-in functions

`len`, `keys`, `values`, `str`, `upper`, `lower`, `join`, `split`,
`contains`, `starts_with`, `ends_with`, `replace`, `matches` (regex), `min`,
`max`, `sort`, `to_json`, `to_yaml`, `b64encode`, `error`. `error(msg)` has type `Never` and
aborts evaluation with `msg`.

## 6. Refinements

`T where φ` restricts `T` to values satisfying the predicate `φ`, in which
`self` is the value. In a field or parameter, the field or parameter's name
may be used instead of `self`. A parameter's predicate may mention earlier
parameters.

```ts
type DnsLabel = Str where len(self) <= 63 && matches(self, "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$")
type Port     = Int where 1 <= self && self <= 65535
fn clamp(lo: Int, hi: Int where hi >= lo, x: Int) -> Int = ...
```

Predicates may use any expression of the language. Every expression is pure
and total, so predicates can always be evaluated.

**Static.** `cleat check` turns each place a value flows into a refined type
into a verification condition (VC). VCs decided by constant folding or by
known facts (path conditions, other refinements) are reported: a refuted VC
is a compile error. All other VCs become runtime checks. An SMT backend may
later decide more of them statically; it is not required.

**Dynamic.** During evaluation, every value that crosses an annotation is
checked against the annotated type: inputs, function arguments and results,
annotated `let`s, and `T { ... }`. These checks enforce both structure and
refinements.

## 7. Checking and evaluation

1. Parse.
2. Resolve types. Check expressions bidirectionally against expected types.
   Diagnostics are field-aware: an unknown field suggests the closest known
   name.
3. Generate VCs (§6).
4. Evaluate, in order: inputs (§8), then top-level `let`s, then `export`s.
   Annotated boundaries check and complete values (fill in defaults, drop
   optional `null`s).
5. Serialize to YAML (multiple documents) or JSON. Strings are always quoted
   where YAML would otherwise read them as something else (`no`, `1.0`, `~`).

## 8. Inputs

An input's value is decoded according to its declared type:

- From a JSON file (`--input values.json`), which holds an object of inputs.
- From the command line (`--set replicas=3`). If the type accepts the raw
  string, the input is that string. Otherwise the string is parsed as JSON.
  So `--set name=web` is a string and `--set replicas=3` is an int.
- From Argo CD (`--argocd`), which reads the `ARGOCD_APP_PARAMETERS`
  environment variable. String parameters are decoded as with `--set`; array
  and map parameters as lists and maps.

A missing input without a default, an unknown input name, or a value that
does not fit the type is an error before evaluation starts. The error names
the input and shows the expected type.

## 9. Components and dependencies

```ts
export db = [ postgres_deployment, postgres_service ]
export config = [ app_config ]
export app after db, config = [ api_deployment, api_service ]
```

A named export is a component. `after` lists the components that must be
healthy before it is applied. Names must exist, a component cannot depend on
itself, and cycles are compile errors that show the cycle (`a -> b -> a`).
Unnamed exports belong to an implicit component called `default` with no
dependencies.

A component's **wave** is 0 if it has no dependencies, otherwise one more
than the largest wave among its dependencies. The output target decides how
dependencies reach the cluster:

| Target | Output |
|---|---|
| `plain` | All documents as one YAML stream, in wave order. |
| `argocd` | As `plain`, with `metadata.annotations["argocd.argoproj.io/sync-wave"]` set to the component's wave. A sync-wave annotation already present in the document is kept. |
| `flux` | `<out>/<component>/resources.yaml` per component, plus `<out>/flux-kustomizations.yaml` with one Flux `Kustomization` per component (`kustomize.toolkit.fluxcd.io/v1`). Each has `path` pointing at its directory, `dependsOn` from `after`, and `wait: true`, so dependents wait for health, not just for the apply. |

## 10. Roadmap

1. ~~Core language, evaluator, YAML/JSON output, typed inputs.~~ Done.
2. ~~Components with `after`; Argo CD sync waves; Flux `dependsOn`.~~ Done.
3. ~~OpenAPI and CRD import (§4.1).~~ Done.
4. `e with { path: v }` typed patches, including list items selected by key
   (`containers[name == "api"]`).
5. Argo CD config-management plugin: a `plugin.yaml`, and parameters
   announced to the Argo CD UI from the `input` declarations.
   (`--argocd-params` already reads `ARGOCD_APP_PARAMETERS`.)
6. Functions and constants in imported modules; nested imports.

## 11. Open questions

- Totality: keep the no-recursion rule, or allow recursion with a fuel limit?
- Should field order in the output follow the schema or the source?
- Strategic-merge semantics for `with` on keyed lists without an explicit
  selector.
- Defaults inside `type` declarations are checked with no variables in scope,
  so they can't refer to inputs or `let`s. Is that the right rule?
- Should components be able to set an explicit wave offset, or Flux options
  like `timeout`, per component?
- The Kubernetes OpenAPI specs (v2 and v3) don't mark enum fields such as
  `ServiceSpec.type`. `refine` (§4.2) covers this per project; should Cleat
  also ship a curated set of refinements for the core API? That needs
  modules that can contain `refine`.
- Recursive types: should aliases be allowed to be recursive, so schemas
  like `JSONSchemaProps` keep their full structure instead of `Json`?
