#!/bin/sh
# Download a Kubernetes OpenAPI spec into schemas/ (not committed).
# Usage: schemas/fetch.sh [version]   (default: 1.31)
set -eu
v=${1:-1.31}
dir=$(cd "$(dirname "$0")" && pwd)
curl -sSfLo "$dir/kubernetes-$v.json" \
  "https://raw.githubusercontent.com/kubernetes/kubernetes/v$v.0/api/openapi-spec/swagger.json"
echo "wrote $dir/kubernetes-$v.json"
