# Getting started

The quickest way to try Cleat is the [playground](playground/), which runs
the real checker in your browser. To use it on your own files, build the
`cleat` command.

## Install

Cleat is built from source for now. [rustup](https://rustup.rs) installs the
Rust version the repository pins in `rust-toolchain.toml`.

```sh
git clone https://github.com/xorps/cleat-lang && cd cleat-lang
cargo install --path crates/cleat      # installs the `cleat` binary
schemas/fetch.sh 1.37                  # the Kubernetes OpenAPI spec, checksum-verified
```

## Your first file

Save this as `hello.cleat`:

```ts
input name: Str
input replicas: Int where self >= 1 = 1

export {
  apiVersion: "v1",
  kind: "ConfigMap",
  metadata.name: "hello-\(name)",
  data.replicas: "\(replicas)",
}
```

Check it, then export it with inputs:

```console
$ cleat check hello.cleat
0 error(s), 0 warning(s); refinements: 1 proved statically, 0 checked at runtime
$ cleat export hello.cleat --set name=world
apiVersion: v1
kind: ConfigMap
metadata:
  name: hello-world
data:
  replicas: "1"
```

`--set` decodes each value by the input's declared type, so
`--set replicas=3` is an integer and `--set replicas=0` is rejected by the
`where` constraint. Inputs can also come from a JSON or YAML file:
`--input values.yaml`.

## Against the Kubernetes API

Import the schema, and resources are checked field by field:

```ts
import * as k8s from "./schemas/kubernetes-1.37.json"

export k8s.ConfigMap {
  metadata.name: "hello",
  data.greeting: "hi",
}
```

See [Kubernetes and CRD types](guides/kubernetes.md) for CRDs and for
narrowing types to your own policies.

## Editor

For VS Code, build and install the extension (no npm needed):

```sh
editors/vscode/package.sh && code --install-extension editors/vscode/cleat.vsix
```

See [Editors](guides/editors.md) for what it does and for other editors.

## Next

- [A tour of the language](tour.md)
- The [examples](https://github.com/xorps/cleat-lang/tree/main/examples) in
  the repository: the quickstart, a web app with its database, and a shared
  base with a production overlay.
