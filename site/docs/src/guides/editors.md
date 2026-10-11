# Editors

`cleat lsp` is a language server on stdio, so any editor with LSP support can
use it. The repository includes a VS Code extension that needs no npm.

## VS Code

```sh
editors/vscode/package.sh && code --install-extension editors/vscode/cleat.vsix
```

Open a `.cleat` file and you get:

- **Errors as you type**, including in files that import the one you're
  editing, and unsaved changes to imported files.
- **Completion** of fields (with Kubernetes documentation), values of enum
  and literal types, variables, functions, modules and types. Required
  fields not yet set come first.
- **Hover** with types and documentation, and **go to definition**, including
  into imported modules and generated schema types.
- **A live preview** of the exported YAML: run **Cleat: Show Output** (the
  preview icon in the editor title bar), and it updates as you type.
- **A resource map**: **Cleat: Show Resource Map** (the hierarchy icon in
  the editor title bar) opens the [`cleat view`](../reference/cli.md)
  dashboard beside the file: the map, tables, checks between resources and
  each resource's details, updating as you type. Selecting a resource jumps
  to where it's defined, such as the `let` that builds it.

### Inputs while editing

Put an inputs file next to the entry file, `app.inputs.json` or
`app.inputs.yaml` beside `app.cleat`:

```json
{ "env": "dev", "image": "ghcr.io/acme/api:1.4.2" }
```

The preview exports with it, and the language server also evaluates with it,
so errors that only show up with real inputs (a runtime refinement check, an
`error(...)` call) appear in the editor at the call site.

### Settings

| Setting | Meaning |
|---|---|
| `cleat.serverPath` | The `cleat` binary. Default: `target/debug/cleat` or `target/release/cleat` in the workspace, else `cleat` on `PATH`. |
| `cleat.compareWith` | A git revision (e.g. `main`) to compare with in the resource map: new, changed and removed resources are marked, as with `cleat view --diff`. |
| `cleat.exportArgs` | Extra `cleat export` arguments for the preview, e.g. `["--set", "env=dev", "--target", "argocd"]`. |

## Other editors

Configure the editor to start `cleat lsp` for `.cleat` files. The server
supports diagnostics, hover, completion and go to definition, with full
document sync.
