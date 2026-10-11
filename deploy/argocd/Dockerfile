# Sidecar image for argocd-repo-server: the cleat binary plus the plugin config.
# Build from the repository root:  docker build -f deploy/argocd/Dockerfile -t cleat-cmp:dev .
# Keep in step with rust-toolchain.toml (copied in, so cargo uses exactly that version).
FROM rust:1.99.0-slim AS build
WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked --bin cleat

FROM debian:bookworm-slim
COPY --from=build /src/target/release/cleat /usr/local/bin/cleat
COPY deploy/argocd/plugin.yaml /home/argocd/cmp-server/config/plugin.yaml
# Argo CD runs plugin sidecars as user 999.
USER 999
