# Command line

```text
usage:
  cleat check  <file.cleat> [--vcs]
  cleat export <file.cleat> [options]
  cleat view   <file.cleat>  the exported resources in a browser, live
  cleat gen <schema>         print Cleat types for an OpenAPI document or CRD file
  cleat lsp                  language server on stdio (used by editors)
  cleat argocd generate      Argo CD plugin: export $ARGOCD_ENV_CLEAT_FILE (default main.cleat)
  cleat argocd params        Argo CD plugin: announce the file's inputs as parameters

options:
  --stdin                  read the source from stdin (the file name still places imports)
  --root <dir>             imports must stay inside this directory
                           (default: nearest directory with .git, else the file's directory)

export options:
  --input <file>           input values: a JSON or YAML object
  --set <name>=<value>     one input (decoded by its declared type); repeatable
  --argocd-params          read inputs from $ARGOCD_APP_PARAMETERS (Argo CD plugin)
  --target <t>             plain (default) | argocd (sync waves) | flux (needs --out)
  --json                   emit JSON instead of YAML
  --out <dir>              output directory (flux)
  --flux-source <name>     GitRepository name (flux; required)
  --flux-path <path>       repository path of --out (flux; default: .)
  --flux-namespace <ns>    namespace for Kustomizations (flux; default: flux-system)
  --flux-prefix <prefix>   prefix for Kustomization names (flux)

view options (also: --input, --set, --target plain|argocd):
  --port <n>               port on 127.0.0.1 (default: any free port)
  --no-open                don't open a browser
  --out <file.html>        write a self-contained snapshot instead of serving
```

## `cleat check`

Parses and type-checks a file and the files it imports, and prints errors
and warnings as `file:line:col: error: message`. It exits with status 1 if
there are errors. `--vcs` also lists each refinement obligation and whether
it was proved, refuted, or left to a runtime check.

## `cleat export`

Checks the file, decodes the inputs, evaluates, and prints the exported
documents as YAML (or JSON with `--json`). Nothing is evaluated if the file
doesn't check, and every bad input is reported before evaluation starts.

Inputs come from `--input <file>` (a JSON or YAML object), `--set name=value`
(repeatable, and decoded by the input's declared type: a string if the type
accepts one, otherwise JSON), or Argo CD's `ARGOCD_APP_PARAMETERS`
(`--argocd-params`). When an input is given more than once, the last one on
the command line wins.

## `cleat view`

Shows the exported resources in your browser, laid out like a cluster
dashboard: resources grouped by kind (Workloads, Network, Config, ...), with
kind-specific columns (replicas and images, Service ports and what they route
to, Ingress routes, ConfigMap keys), a namespace filter and search, and the
sync order of components. Selecting a resource shows its containers, labels,
relations to other resources and its YAML.

The **Map** draws the resources as a graph, left to right in the order
traffic and configuration flow: Ingresses, then Services (and autoscalers),
then workloads, then the ConfigMaps, Secrets, volume claims and service
accounts they use, with one box per namespace. References to resources that
aren't in the output appear as dashed nodes, so a broken link is visible
where it is. Hovering a resource highlights what it's connected to.

It also checks the resources against each other, which the type checker
can't: a Service that selects no workload, a named `targetPort` no selected
container has, an Ingress or HorizontalPodAutoscaler pointing at something
that isn't there, and ConfigMaps, Secrets and volume claims that are
referenced but not in the output (which may be fine, if they already exist
in the cluster).

The page updates within a second when the file, or anything it imports,
changes. Inputs start from `--set` and `--input` and can be edited on the
page. The server listens on 127.0.0.1 only. With `--out view.html` it writes
a self-contained snapshot instead, to attach to a pull request or a ticket.

## `cleat gen`

Prints the Cleat types generated from an OpenAPI document or a CRD file,
which is what an `import * as ns from "schema"` sees.

## Project root

Imports can't leave the project root: the nearest directory above the entry
file that contains `.git`, else the file's own directory, or `--root`.
Symbolic links are resolved before the check.
