# AGENTS.md

Guidance for coding agents (and humans) working in this repository.

Cleat is a typed configuration language whose values are exactly JSON, aimed
at replacing Helm and Kustomize for Kubernetes. It is a Cargo workspace plus
a dependency-free VS Code extension and an Argo CD plugin:

- `crates/cleat-core`: the language (lexer to output). `no_std` + `alloc`,
  and no I/O: files are read through a `Host` the application supplies.
- `crates/cleat`: the `cleat` binary and library on a real file system (CLI,
  language server, Argo CD plugin). Its `FsHost` reads files and parses YAML.
- `crates/cleat-wasm`: the core compiled to WebAssembly for the browser
  playground, with files held in memory.
- `site/`: the website on GitHub Pages: mdBook docs (`site/docs`) and the
  playground page (`site/playground`, plain HTML/CSS/JS, no build step).

[SPEC.md](SPEC.md) is the source of truth for language behavior; read the
relevant section before changing semantics.

## Commands

```sh
cargo build                                   # toolchain comes from rust-toolchain.toml (Rust 1.99.0)
cargo test                                    # crates/cleat/tests/check.rs and crates/cleat-core/tests/
cargo fmt                                     # rustfmt, max_width = 120 (rustfmt.toml)
cargo clippy --all-targets -- -D warnings     # CI fails on any warning
schemas/fetch.sh 1.37                         # Kubernetes schema; tests that need it skip without it
cargo clippy -p cleat-core --target thumbv7em-none-eabihf   # proves the core needs no std (rustup target add it once)

cargo run -- check examples/webapp.cleat      # check a file
cargo run -- export examples/quickstart.cleat --set env=prod --set image=x
cargo run -- gen schemas/kubernetes-1.37.json # print the types generated from a schema
cargo run -- complete <file> <line> <col>     # debug completion (0-based line/col)

site/build.sh                                 # docs + playground into _site/ (pinned, verified mdBook)
python3 -m http.server -d _site 8000          # preview the site at http://localhost:8000/
editors/vscode/package.sh                     # build cleat.vsix (no npm); node --check the extension
deploy/argocd/e2e/run.sh                      # real Argo CD on a throwaway kind cluster (needs docker, kubectl)
deploy/argocd/e2e/run.sh cleanup              # delete that cluster
```

In CI the Argo CD end-to-end test (`.github/workflows/argocd-e2e.yml`) is slow,
so it runs weekly, on demand, and on pull requests labeled `e2e` (and on each
push to them while the label is on). Add the label to changes that touch the
plugin, the sidecar image, inputs or output targets.

Before finishing a change, run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, the no-std build of the core above, and check the examples (`examples/*.cleat`, `examples/overlays/prod.cleat`).

## Code map

The pipeline is: source → `lexer` → `parser` → AST → `check` (static) → `eval` → `targets` → `emit`.
Paths below are in `crates/cleat-core/src/` unless they say otherwise.

| File | What it does |
|---|---|
| `host.rs` | The `Host` trait (resolve, stamp, read, parse YAML), the `Session` that holds every cache, `MemHost` |
| `lexer.rs` | Tokens, string interpolation `\(...)`, `"""` multi-line strings |
| `parser.rs` | Recursive-descent parser; dotted keys are desugared into nested objects here |
| `ast.rs` | AST, `Expr` printing (used in messages) and substitution |
| `types.rs` | `Ty`, `TypeCtx` (aliases, resolution), normalization, subtyping, narrowing, type-level `with` |
| `check.rs` | Static checker: bidirectional checking, patterns, imports, value patches, hovers, definitions, completion hooks |
| `vc.rs` | Refinement obligations: proved, refuted (a compile error), or left to the runtime check |
| `eval.rs` | Evaluator, `conform` (runtime type checks that fill defaults), input decoding, builtins |
| `modules.rs` | Import loading through the host, caching by stamp, dependency recording |
| `openapi.rs` | OpenAPI/CRD → Cleat type source; the curated `KUBERNETES_ENUMS` table |
| `targets.rs` | Output targets: `plain`, `argocd` (sync waves), `flux` (`dependsOn`) |
| `diff.rs` | Line diffs (LCS) and unified-diff hunks of rendered YAML |
| `emit.rs`, `json.rs` | YAML/JSON output with safe quoting; JSON input |
| `complete.rs` | Completion probes and syntactic go-to-definition |
| `crates/cleat/src/fs.rs` | `FsHost`: files on disk, the editor-buffer overlay, path confinement after symlinks, project roots, inputs files |
| `crates/cleat/src/lib.rs` | The `Path`-based API on a per-thread `FsHost` session |
| `crates/cleat/src/yaml.rs` | YAML input (CRDs, data imports, inputs files) |
| `crates/cleat/src/lsp.rs` | Language server (stdio JSON-RPC, no LSP crate) |
| `crates/cleat/src/view.rs`, `view/` | `cleat view`: a small localhost server (and `--out` snapshots) for the dashboard page in `view/` (plain HTML/CSS/JS, embedded with `include_str!`) |
| `crates/cleat/src/compare.rs`, `git.rs` | `cleat diff` / `view --diff`: evaluating on any session, comparing outputs, and `GitHost` (files at a revision; git-ignored files from the working tree) |
| `crates/cleat/src/main.rs` | CLI, including `cleat argocd generate|params` for the Argo CD plugin |
| `crates/cleat-wasm/src/lib.rs` | The playground's WebAssembly interface: one JSON-in, JSON-out call over a `MemHost` |
| `site/playground/` | The playground page: editor, highlighting, hover, completion, inputs, share links. Its Resources tab embeds the `cleat view` page (`?embed=1`, state sent by `postMessage`), which `site/build.sh` copies from `crates/cleat/src/view/` |
| `site/docs/` | The documentation (mdBook); `reference/spec.md` includes `SPEC.md` |
| `editors/vscode/` | Extension: TextMate grammar, a small LSP client, live preview, and the resource map (a webview of `cleat view --page`, fed by `cleat view --state`, under a nonce-based CSP) |
| `deploy/argocd/` | Plugin config, distroless sidecar Dockerfile, repo-server patch, e2e test |

## Design rules to preserve

These are deliberate; changing them is a design decision, not a fix.

- **Values are JSON.** No other values reach output.
- **The core is `no_std` and sans-I/O.** `cleat-core` never touches files,
  the clock, the environment or global state: reads go through
  `Session::host()`, and caches live in the `Session`, never in statics or
  thread-locals. Anything that needs `std` belongs in a host crate. CI builds
  the core for a target that has no `std` at all.
- **Evaluation terminates and is hermetic.** No recursion (a function may only
  call functions declared above it), no environment, clock or network. The
  only file access is through `import`.
- **Imports are static.** Paths are string literals, all imports are resolved
  before evaluation (checker and evaluator both hoist them), and every
  imported file must resolve inside the project root, after symlinks
  (`Host::resolve`; see `FsHost` and `MemHost`).
- **`with` and `refine` only narrow.** A replacement type must be a subtype;
  only optional fields can be removed. Refined types stay valid against the
  upstream schema.
- **The checker and the evaluator must agree.** A new construct needs both
  static typing (`check.rs`) and evaluation (`eval.rs`). Annotated boundaries
  are re-checked at runtime by `conform`.
- **Errors are the product.** Messages name the field or path and the type,
  quote code in backticks, and suggest a fix where possible (`suggest()` gives
  "did you mean"). One root cause should produce one error: return
  `Ty::Error` (compatible with everything) after reporting, and don't report
  into namespaces whose import failed (`failed_namespaces`).

## Pitfalls

- **Resolve a `Ty::Alias` before using it.** `TypeCtx::unfold` returns
  `Ty::Error` for an alias that hasn't been resolved, and `Ty::Error` accepts
  everything, so the check silently passes. Whenever you create a `Ty::Alias`
  by hand (typed literals, imported signatures), call `ctx.resolve` on it
  first. This bug has happened twice.
- **`refine` must run before imported types are resolved.** Resolving
  caches a type, and a cached type no longer sees a later `refine`, which
  then silently does nothing. The checker hoists imports, applies all
  `refine`s, and only then resolves the types that imported signatures and
  constants mention (`unresolved_imports`). A file imported under two names
  shares one set of types (`share_types`).
- **Spans are 1-based line/column in characters.** The language server
  converts to LSP's 0-based UTF-16 positions in `lsp.rs`.
- **Completion works on unfinished code** by checking a probe: the text up to
  the cursor with a placeholder (`check::COMPLETE_NAME` / `COMPLETE_VALUE`) and
  closed brackets. The checker records candidates when it meets a
  placeholder. New syntax positions may need a hook there.
- **Caches are keyed by what was read.** `modules::load` and
  `check::checked_module` record every file they read (`Session::record`);
  anything new that reads files must go through `modules::read_source` /
  `load` / `load_data` so editor buffers and invalidation keep working.
- **The core has no `std` prelude.** Each module starts with
  `use crate::prelude::*;` (`String`, `Vec`, `format!`, `BTreeMap`, ...).
  Use `BTreeMap`/`BTreeSet`, not hash maps, and avoid float methods that need
  `libm` (`fract`, `floor`, `powf`, ...).
- **Some tests need the Kubernetes schema** (`schemas/kubernetes-1.37.json`,
  not committed). They print "skipping" without it; run `schemas/fetch.sh 1.37`
  so they actually run.
- **Validate workflow YAML strictly** after editing `.github/`:
  `ruby -ryaml -e 'YAML.load_file(ARGV[0])' .github/workflows/ci.yml`.
  Cleat's own YAML reader is more lenient than GitHub's (for example about
  `: ` inside unquoted values), so it isn't a reliable check here.
- **The sidecar image is distroless**: no shell. Inspect it with
  `docker image inspect` and `docker cp`, not `docker run ... sh`.

## Tests

Most tests are in `crates/cleat/tests/check.rs` and call the library
directly; `crates/cleat-core/tests/` tests the core on a `MemHost` (no file
system). Use the
helpers there: `ok(src)` and `err(src, "message substring")` for checking
(with a small Kubernetes-like prelude), `run`/`yaml` for evaluation,
`fixture_dir` for tests that need files (imports, schemas), and
`completions` for editor completion (the cursor is marked with `‸`).
Assert on the actual message text, since messages are part of the interface.
Add a test for every bug fix.

## Documentation

- Language behavior: update the relevant section of [SPEC.md](SPEC.md).
- User-facing changes: update [README.md](README.md) and the guides in
  `site/docs/src/`. Code and output in the docs are real: check snippets
  with `cleat check` and paste actual output. Its first example is
  `examples/quickstart.cleat`, and its output blocks are real output; keep
  them in sync if messages change.
- Keep the examples passing `cleat check`.

## Supply chain

- GitHub Actions are pinned to full commit SHAs with the version in a
  comment; Dependabot updates them.
- Docker base images and the Dockerfile frontend are pinned by digest.
- Downloads are checksum-verified: Kubernetes schemas against
  `schemas/SHA256SUMS`, `kind` and the Argo CD manifest in
  `deploy/argocd/e2e/run.sh`, and mdBook in `site/build.sh`. When adding a
  schema version, review the file and add its checksum.
- Updating Rust means changing `rust-toolchain.toml` and the `rust:<version>`
  image in `deploy/argocd/Dockerfile`.

## Allocation: current state and future direction

Today the core uses `alloc`'s global allocator (`Vec`, `String`, `BTreeMap`,
`Rc`) and aborts on allocation failure, like most Rust code. Each final
binary chooses the allocator: the CLI uses the system one, and the
WebAssembly build uses Rust's default for `wasm32`.

The direction, to revisit later, is explicit and fallible allocation that
works on stable Rust without extra crates:

- Keep the AST, types and values in stores the caller owns and passes in
  explicitly (arenas indexed by small IDs instead of `Box`/`Rc` graphs).
- Grow them with `Vec::try_reserve` and report `OutOfMemory` as an ordinary
  error, with a small map of our own instead of `BTreeMap`, and a fallible
  `fmt::Write` sink for building strings.
- Leave `#[global_allocator]` to the leaf crate, which can then cap memory
  per evaluation (useful in the Argo CD sidecar and the browser).
- When `allocator_api` stabilizes, move the stores to `Vec<T, A>` so the
  allocator itself is passed explicitly.

The alternative considered was an allocator trait of our own with
collections written over raw memory. It's more control for much more unsafe
code, so it is not the plan.

## Commits

Small, focused commits with an imperative summary line and a body that
explains why, signed off (`git commit -s`, see [CONTRIBUTING.md](CONTRIBUTING.md)).
The project is Apache-2.0; contributions are accepted as `Apache-2.0 OR MIT`. Don't commit `target/`, downloaded schemas
(`schemas/*.json`), or `editors/vscode/cleat.vsix`.
