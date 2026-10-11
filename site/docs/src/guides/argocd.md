# Argo CD

Cleat runs as an Argo CD
[config management plugin](https://argo-cd.readthedocs.io/en/stable/operator-manual/config-management-plugins/):
a sidecar container next to `argocd-repo-server`. The sidecar image is about
6 MB: a statically linked `cleat` and the plugin config on distroless, with
no shell.

## Install the sidecar

Build and push the image (multi-architecture builds need no emulation):

```sh
docker buildx build -f deploy/argocd/Dockerfile --platform linux/amd64,linux/arm64 \
  -t registry.example.com/cleat-cmp:dev --push .
```

Then add the sidecar to the repo server. Edit the image in
`deploy/argocd/repo-server-patch.yaml` to the one you pushed, and apply it:

```sh
kubectl -n argocd patch deployment argocd-repo-server --patch-file deploy/argocd/repo-server-patch.yaml
```

The image carries its plugin config (`deploy/argocd/plugin.yaml`), so nothing
else needs to be installed. The plugin is called `cleat-v1`, and it also
matches, without being named, any application whose directory has a
`main.cleat`.

## An Application

```yaml
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata: { name: shop, namespace: argocd }
spec:
  project: default
  source:
    repoURL: https://github.com/acme/deploy.git
    targetRevision: main
    path: apps/shop
    plugin:
      name: cleat-v1
      parameters:
        - { name: env, string: prod }
        - { name: image, string: ghcr.io/acme/api:1.4.2 }
  destination: { server: https://kubernetes.default.svc, namespace: shop }
```

| Plugin env (`spec.source.plugin.env`) | Meaning |
|---|---|
| `CLEAT_FILE` | The entry file in the application's directory (default `main.cleat`). |
| `CLEAT_TARGET` | `argocd` (default, adds sync waves) or `plain`. |

## What you get

- **Parameters in the UI.** Each `input` in the entry file is announced to
  Argo CD as a parameter, with its type as the tooltip and its default
  filled in.
- **Typed parameters.** String parameters are decoded by the input's type,
  as with `--set`; array and map parameters become lists and maps.
- **Errors in the Application's status.** An invalid parameter or a type
  error fails the manifest generation with Cleat's message, for example
  ``input `env`: expected `Env` (`"dev" | "prod"`), got "production" (string)``.
- **Sync waves** from `after` dependencies (see
  [Components](components.md)).
- **Imports stay in the repository.** The project root is the repository
  checkout, so an application can import shared modules and schemas from
  elsewhere in the repository, and nothing outside it.

## Testing it

`deploy/argocd/e2e/run.sh` creates a throwaway kind cluster, installs a
pinned Argo CD, builds the sidecar, and checks all of the above against a
real Argo CD. `deploy/argocd/e2e/run.sh cleanup` deletes the cluster. In
the repository's CI it runs weekly, on demand, and on pull requests labeled
`e2e`.
