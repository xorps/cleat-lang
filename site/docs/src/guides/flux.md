# Flux

Flux applies plain manifests from Git, so with Flux you run `cleat export`
in CI (or before committing) and commit the output.

```sh
cleat export app.cleat --target flux --out deploy --flux-source shop --flux-path deploy
```

This writes a directory per [component](components.md) and one file of Flux
`Kustomization`s:

```text
deploy/app/resources.yaml
deploy/config/resources.yaml
deploy/database/resources.yaml
deploy/flux-kustomizations.yaml
```

Each `Kustomization` points at its component's directory, depends on the
components in its `after` list, and has `wait: true`, so dependents wait
until their dependencies are healthy rather than just applied:

```yaml
apiVersion: kustomize.toolkit.fluxcd.io/v1
kind: Kustomization
metadata:
  name: app
  namespace: flux-system
spec:
  interval: 10m
  path: ./deploy/app
  prune: true
  wait: true
  sourceRef:
    kind: GitRepository
    name: shop
  dependsOn:
    - name: database
    - name: config
```

| Option | Meaning |
|---|---|
| `--out <dir>` | Where to write (required). |
| `--flux-source <name>` | The `GitRepository` the Kustomizations read from (required). |
| `--flux-path <path>` | The repository path of `--out` (default `.`). |
| `--flux-namespace <ns>` | Namespace of the Kustomizations (default `flux-system`). |
| `--flux-prefix <prefix>` | Prefix for the Kustomization names. |

Apply `flux-kustomizations.yaml` once (or include it from your cluster's
Flux configuration), and Flux picks up changes to the component
directories from then on.
