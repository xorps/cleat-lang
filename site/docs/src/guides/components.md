# Components and dependencies

A named export is a **component**. `after` lists the components that must be
healthy before it is applied:

```ts
export config = [settings]
export database = [postgres]
export app after database, config = [api]
```

Unknown names and cycles are compile errors:

```console
$ cleat check cycle.cleat
cycle.cleat:1:1: error: dependency cycle between components: a -> b -> a
```

Unnamed exports belong to a component called `default`, with no
dependencies.

## Waves

A component's wave is 0 if it has no dependencies, otherwise one more than
the largest wave among its dependencies. Above, `config` and `database` are
wave 0 and `app` is wave 1.

## How dependencies reach the cluster

The output target decides:

| Target | Output |
|---|---|
| `plain` (default) | One YAML stream, in wave order. |
| `argocd` | The same, with the `argocd.argoproj.io/sync-wave` annotation set on every document (an existing one is kept). See [Argo CD](argocd.md). |
| `flux` | A directory per component and a Flux `Kustomization` for each, with `dependsOn`. See [Flux](flux.md). |

```console
$ cleat export app.cleat --target argocd
apiVersion: v1
kind: ConfigMap
metadata:
  name: settings
  annotations:
    argocd.argoproj.io/sync-wave: "0"
...
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: api
  annotations:
    argocd.argoproj.io/sync-wave: "1"
```
