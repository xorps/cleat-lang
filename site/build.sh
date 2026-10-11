#!/bin/sh
# Build the website: the docs (mdBook) at the root and the playground (Cleat
# compiled to WebAssembly) under playground/. GitHub Pages serves the result.
# Usage: site/build.sh [out-dir]      (default: _site; needs schemas/fetch.sh 1.37 first)
# Preview: python3 -m http.server -d _site 8000, then open http://localhost:8000/
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$repo/_site}
schema=kubernetes-1.37.json

MDBOOK_VERSION=v0.5.4
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
verify() { # file expected-sha256
  got=$(sha256 "$1")
  [ "$got" = "$2" ] || { echo "error: checksum mismatch for $1: got $got, want $2" >&2; exit 1; }
}

# mdBook: a pinned release, checksum-verified, cached in $TMPDIR.
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) triple=x86_64-unknown-linux-musl; want=5222beabd3e37dc5be0d18ff99b79058469354db5c220153a1b92db5ba12be89 ;;
  Linux-aarch64) triple=aarch64-unknown-linux-musl; want=753e5c5c363ee8a56972344dcf91466f005a51db84a7aeffe427ae3ef83d6d44 ;;
  Darwin-x86_64) triple=x86_64-apple-darwin; want=a47d7bf0d5d670cff9ee6cce95537cbeb62dc10704d9e7131ffbd13e2b59a5de ;;
  Darwin-arm64) triple=aarch64-apple-darwin; want=03e8a6d8b13a2971e0b3280affd03b388373c1485e26f73407c3a76b0b1838df ;;
  *) echo "unsupported platform for mdBook: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
mdbook_dir=${TMPDIR:-/tmp}/mdbook-$MDBOOK_VERSION-$triple
if [ ! -x "$mdbook_dir/mdbook" ]; then
  mkdir -p "$mdbook_dir"
  curl -sSfLo "$mdbook_dir/mdbook.tar.gz" \
    "https://github.com/rust-lang/mdBook/releases/download/$MDBOOK_VERSION/mdbook-$MDBOOK_VERSION-$triple.tar.gz"
  verify "$mdbook_dir/mdbook.tar.gz" "$want"
  tar -xzf "$mdbook_dir/mdbook.tar.gz" -C "$mdbook_dir" mdbook
fi

[ -f "$repo/schemas/$schema" ] || { echo "error: run schemas/fetch.sh 1.37 first" >&2; exit 1; }
verify "$repo/schemas/$schema" "$(awk -v n="$schema" '$2 == n { print $1 }' "$repo/schemas/SHA256SUMS")"

echo "==> playground (WebAssembly)"
rustup target add wasm32-unknown-unknown >/dev/null
cargo build --locked -p cleat-wasm --profile wasm --target wasm32-unknown-unknown --manifest-path "$repo/Cargo.toml"

echo "==> docs (mdBook)"
"$mdbook_dir/mdbook" build "$repo/site/docs" --dest-dir "$out"

pg=$out/playground
mkdir -p "$out/assets" "$pg/examples/overlays" "$pg/schemas"
cp "$repo"/assets/*.svg "$out/assets/"
cp "$repo"/site/playground/index.html "$repo"/site/playground/playground.css "$repo"/site/playground/playground.js \
  "$repo"/site/playground/basics.cleat "$pg/"
cp "$repo/target/wasm32-unknown-unknown/wasm/cleat_wasm.wasm" "$pg/cleat.wasm"
cp "$repo/examples/quickstart.cleat" "$repo/examples/webapp.cleat" "$pg/examples/"
cp "$repo/examples/overlays/base.cleat" "$repo/examples/overlays/prod.cleat" "$pg/examples/overlays/"
cp "$repo/schemas/$schema" "$pg/schemas/"
touch "$out/.nojekyll"
echo "wrote $out"
