# Contributing to Cleat

Thanks for your interest! Cleat is under active development, so expect
things to move. Opening an issue to discuss larger changes before writing
code is the fastest way to get them in.

## Getting set up

[AGENTS.md](AGENTS.md) has everything needed to build, test and change the
code: commands, a map of the source, the design rules a change must keep, and
known pitfalls. In short:

```sh
schemas/fetch.sh 1.37                       # so schema-dependent tests run
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Language behavior is specified in [SPEC.md](SPEC.md); changes to it should
update the spec in the same pull request.

## Pull requests

- Keep each pull request focused, with tests for what it changes.
- Write commit messages with an imperative summary and a body that explains why.
- Sign off every commit (see below).

## Developer Certificate of Origin

Contributions are accepted under the
[Developer Certificate of Origin](https://developercertificate.org/) (DCO).
By signing off a commit you certify that you wrote it, or otherwise have the
right to submit it under the license terms below. Sign off with `git commit -s`,
which adds a line like:

```
Signed-off-by: Your Name <you@example.com>
```

## Licensing of contributions

Cleat is distributed under the [Apache License 2.0](LICENSE).

Unless you explicitly state otherwise, any contribution you intentionally
submit for inclusion in Cleat, as defined in the Apache License 2.0, is
licensed by you under **both** the Apache License 2.0 **and** the MIT license
(SPDX: `Apache-2.0 OR MIT`), without any additional terms or conditions.

You don't pick one: by contributing, you grant both. The project, and anyone
who receives your contribution from it, may then use it under either license.
Cleat itself is currently distributed under Apache-2.0 only; accepting
contributions under both keeps the option open to also offer Cleat under MIT
later without having to ask every past contributor again.

Copyright The Cleat Authors.
