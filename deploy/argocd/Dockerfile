# syntax=docker/dockerfile:1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e

# Sidecar image for argocd-repo-server: a statically linked `cleat` and the
# plugin config on distroless (no shell, no libc, no package manager).
#
# Build from the repository root (multi-arch works without emulation):
#   docker build -f deploy/argocd/Dockerfile -t cleat-cmp:dev .
#   docker buildx build -f deploy/argocd/Dockerfile --platform linux/amd64,linux/arm64 .

# The build stage runs on the build machine and cross-compiles for the image's platform.
FROM --platform=$BUILDPLATFORM rust:1.99.0-slim@sha256:2752b332db73fdbb7dc576f06c82ed1f312005784ef913d7e04a28f5f55dc581 AS build
ARG TARGETARCH
WORKDIR /src

# The toolchain from rust-toolchain.toml plus the musl target for the image's
# architecture. This layer is reused until rust-toolchain.toml changes.
RUN --mount=type=bind,source=rust-toolchain.toml,target=rust-toolchain.toml \
    case "$TARGETARCH" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && rustup toolchain install \
    && rustup target add "$arch-unknown-linux-musl" \
    && echo "$arch-unknown-linux-musl" > /target-triple

# Sources are bind-mounted rather than copied, and the registry and target
# directories are caches; only the finished binary is written into the layer.
# musl binaries are static; Rust's bundled lld links them for any architecture.
RUN --mount=type=bind,source=rust-toolchain.toml,target=rust-toolchain.toml \
    --mount=type=bind,source=Cargo.toml,target=Cargo.toml \
    --mount=type=bind,source=Cargo.lock,target=Cargo.lock \
    --mount=type=bind,source=src,target=src \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    target="$(cat /target-triple)" \
    && RUSTFLAGS="-C linker=rust-lld" cargo build --release --locked --bin cleat --target "$target" \
    && cp "target/$target/release/cleat" /cleat

FROM gcr.io/distroless/static-debian13@sha256:58133991db06659feaabe0f4e97a35cebf15ef4ea08f8a4c6d2ee5f75e4aa6a0
COPY --from=build /cleat /usr/local/bin/cleat
COPY deploy/argocd/plugin.yaml /home/argocd/cmp-server/config/plugin.yaml
# Argo CD runs plugin sidecars as user 999.
USER 999
