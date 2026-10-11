# How Cleat is built

Cleat is written in Rust as a small workspace:

| Crate | What it is |
|---|---|
| `cleat-core` | The language: lexer, parser, checker, evaluator, schema import and output. `no_std` with `alloc`, and no I/O. |
| `cleat` | The `cleat` command, the language server and the Argo CD plugin, on a real file system. |
| `cleat-wasm` | `cleat-core` compiled to WebAssembly, for the [playground](../playground/). |

## A core without I/O

`cleat-core` never reads a file, the clock or the environment. Everything
outside the program text goes through a `Host` that the application
provides:

```rust
pub trait Host {
    /// The file an import names, confined to the project root.
    fn resolve(&self, base: Option<&str>, root: Option<&str>, spec: &str) -> Result<String, String>;
    /// A version of a file's contents, so caches know when it changed.
    fn stamp(&self, file: &str) -> Stamp;
    fn read(&self, file: &str) -> Result<String, String>;
    fn parse_yaml(&self, text: &str) -> Result<Vec<Value>, String>;
    // ...
}
```

The command line uses a file-system host, whose overlay lets the language
server see unsaved editor buffers. The playground uses an in-memory host.
Because the core can't do anything else, evaluation is hermetic by
construction: the same files give the same output.

A `Session` holds a host together with its caches (loaded and checked
modules, compiled regular expressions), keyed by the stamps of every file a
result was computed from. The core is built in CI for a target that has no
`std` at all, so it stays portable.

## Allocation

Today the core uses the global allocator through `alloc`'s collections, and
each final program chooses that allocator. The plan for later is explicit,
fallible allocation on stable Rust: the AST, types and values in arenas the
caller owns and passes in, grown with `try_reserve` so running out of memory
is an ordinary error. That would let the Argo CD sidecar and the browser cap
the memory of one evaluation. When Rust's allocator API stabilizes, the
allocator itself can be passed explicitly.

## Supply chain

GitHub Actions are pinned to commit SHAs and updated by Dependabot, base
images are pinned by digest, and every download (Kubernetes schemas, kind,
Argo CD, mdBook) is checked against a recorded SHA-256.
