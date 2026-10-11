# Kubernetes and CRD types

Cleat reads the Kubernetes OpenAPI spec and CRD files directly and turns them
into Cleat types. Nothing is generated into your repository.

## The Kubernetes API

```sh
schemas/fetch.sh 1.37    # downloads the 1.37 spec and verifies its checksum
```

```ts
import * as k8s from "../schemas/kubernetes-1.37.json"

export k8s.Service {
  metadata.name: "api",
  spec: {
    selector.app: "api",
    ports: [{ port: 80, targetPort: 8080 }],
  },
}
```

Each resource has a short name (`k8s.Deployment`, `k8s.Service`) and its
full name (`k8s.apps.v1.Deployment`). When a kind exists in several API
versions, the short name is the most stable one. Resource types fill in
their own `apiVersion` and `kind`.

What's checked:

- every field name, with a "did you mean" suggestion for typos;
- required fields;
- value types, including int-or-string fields (`targetPort: 8080` or
  `"http"`) and quantities (`cpu: "500m"` or `cpu: 0.5`);
- enums: those in the spec, plus about 40 that the spec leaves as plain
  strings, like `Service.spec.type`, `imagePullPolicy` and `restartPolicy`;
- patterns from the schema (for example on names), as regex refinements.

Field documentation from the spec appears on hover and in completion.
`cleat gen schemas/kubernetes-1.37.json` prints all the generated types.

Named imports bring individual types into scope:

```ts
import { Deployment, Service } from "../schemas/kubernetes-1.37.json"
```

## CRDs

Import CRD manifests, as YAML or JSON, the same way:

```ts
import * as cm from "./crds/cert-manager.crds.yaml"

export cm.Certificate {
  metadata.name: "web-tls",
  spec: {
    secretName: "web-tls",
    dnsNames: ["example.com"],
    issuerRef: { name: "letsencrypt", kind: "ClusterIssuer" },
  },
}
```

A CRD's storage version gets the short name; other versions are available
by their full names.

## Your own policies: `with` and `refine`

Schemas describe what an API accepts, which is usually looser than your
project's policy. `T with { ... }` makes a narrower copy of a type:

```ts
type SmallDeployment = k8s.Deployment with {
  spec.replicas where self <= 10,                                    // add a predicate
  spec.template.spec.containers[].image where contains(self, ":"),   // `[]`: each element
  spec.strategy.type: "RollingUpdate" | "Recreate",                  // replace the type
  spec.revisionHistoryLimit: Int = 3,                                // ... and set a default
  -status,                                                           // remove an optional field
}
```

`refine T with { ... }` narrows a type in place, so every use of it in the
file, including inside other imported types, sees the narrower version:

```ts
// No NodePort or ExternalName Services in this project.
refine k8s.ServiceSpec with { type: "ClusterIP" | "LoadBalancer" }
```

Both only narrow: a replacement type must be a subtype of the original, only
optional fields can be removed, and a field stays required or optional as it
was. So anything that satisfies your refined type is still valid Kubernetes.
