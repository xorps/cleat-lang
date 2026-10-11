#!/bin/sh
# Download a Kubernetes OpenAPI spec into schemas/ (not committed) and verify it
# against schemas/SHA256SUMS.
# Usage: schemas/fetch.sh [version]   (default: 1.31)
set -eu
v=${1:-1.31}
dir=$(cd "$(dirname "$0")" && pwd)
name="kubernetes-$v.json"
tmp="$dir/$name.download"
curl -sSfLo "$tmp" "https://raw.githubusercontent.com/kubernetes/kubernetes/v$v.0/api/openapi-spec/swagger.json"

want=$(awk -v n="$name" '$2 == n { print $1 }' "$dir/SHA256SUMS")
got=$(if command -v sha256sum >/dev/null; then sha256sum "$tmp"; else shasum -a 256 "$tmp"; fi | cut -d' ' -f1)
if [ -z "$want" ]; then
  echo "warning: no checksum recorded for $name; after reviewing it, add this line to schemas/SHA256SUMS:" >&2
  echo "$got  $name" >&2
elif [ "$got" != "$want" ]; then
  rm -f "$tmp"
  echo "error: checksum mismatch for $name: got $got, want $want" >&2
  exit 1
fi
mv "$tmp" "$dir/$name"
echo "wrote $dir/$name"
