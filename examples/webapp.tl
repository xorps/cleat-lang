// A web app and its database as Kubernetes manifests.
//
//   tl export examples/webapp.tl --set env=prod --set image=ghcr.io/acme/api:1.4.2
//   tl export examples/webapp.tl --set env=dev --set image=api:dev --target argocd
//
// The Kubernetes types below are hand-written; they will come from OpenAPI import.

type Env      = "dev" | "staging" | "prod"
type DnsLabel = Str where len(self) <= 63 && matches(self, "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$")
type Port     = Int where 1 <= self && self <= 65535
type Labels   = { [Str]: Str }

type Metadata = { name: DnsLabel, namespace?: DnsLabel, labels?: Labels, annotations?: Labels }

type Container = {
  name: DnsLabel,
  image: Str,
  ports?: [{ name?: Str, containerPort: Port, protocol: "TCP" | "UDP" = "TCP" }],
  env?: [{ name: Str, value: Str }],
  resources?: { requests?: { [Str]: Str }, limits?: { [Str]: Str } },
}

type Deployment = {
  apiVersion: "apps/v1",
  kind: "Deployment",
  metadata: Metadata,
  spec: {
    replicas: Int where self >= 0 = 1,
    selector: { matchLabels: Labels },
    template: { metadata: { labels: Labels }, spec: { containers: [Container] } },
  },
}

type Service = {
  apiVersion: "v1",
  kind: "Service",
  metadata: Metadata,
  spec: {
    type: "ClusterIP" | "NodePort" | "LoadBalancer" = "ClusterIP",
    selector: Labels,
    ports: [{ name?: Str, port: Port, targetPort: Port | Str }],
  },
}

type ConfigMap = { apiVersion: "v1", kind: "ConfigMap", metadata: Metadata, data: { [Str]: Str } }

// ---- inputs: typed, decoded and validated before anything is evaluated ----

input env: Env
input image: Str
input name: DnsLabel = "api"
input replicas: Int where self >= 1 = 2
input debug: Bool = false

// ---- building blocks ----

fn labels(app: Str) -> Labels = {
  "app.kubernetes.io/name": app,
  "app.kubernetes.io/part-of": name,
}

fn deployment(app: DnsLabel, image: Str, port: Port, replicas: Int where self >= 1 = 1, vars: { [Str]: Str } = {}) -> Deployment =
  Deployment {
    metadata: { name: app, labels: labels(app) },
    spec: {
      replicas,
      selector.matchLabels: labels(app),
      template: {
        metadata.labels: labels(app),
        spec.containers: [{
          name: app,
          image,
          ports: [{ name: "http", containerPort: port }],
          env: [for k, v in vars => { name: k, value: v }],
          if env == "prod" then { resources.limits: { cpu: "1", memory: "512Mi" } },
        }],
      },
    },
  }

fn service(app: DnsLabel, port: Port) -> Service = Service {
  metadata: { name: app, labels: labels(app) },
  spec: { selector: labels(app), ports: [{ name: "http", port, targetPort: "http" }] },
}

// ---- components: `after` becomes Argo CD sync waves or Flux dependsOn ----

export db = [
  deployment("postgres", "postgres:16", 5432, vars: { POSTGRES_DB: name }),
  service("postgres", 5432),
]

export config = [
  ConfigMap {
    metadata.name: "\(name)-config",
    data: { LOG_LEVEL: if debug then "debug" else "info", ENVIRONMENT: env },
  },
]

export app after db, config = [
  deployment(
    name,
    image,
    8080,
    replicas: if env == "prod" then max(replicas, 3) else replicas,
    vars: { DATABASE_HOST: "postgres", CONFIG: "\(name)-config" },
  ),
  service(name, 8080),
]
