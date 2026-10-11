#!/bin/sh
# End-to-end test of the Cleat Argo CD plugin on a throwaway kind cluster.
#
#   deploy/argocd/e2e/run.sh            # create cluster, install, sync, verify
#   deploy/argocd/e2e/run.sh cleanup    # delete the cluster
#
# Needs docker and kubectl. Downloads a pinned kind (checksum-verified) and
# installs a pinned Argo CD release (manifest checksum-verified).
set -eu
cd "$(dirname "$0")/../../.."
CLUSTER=cleat-e2e
CTX=kind-$CLUSTER
k() { kubectl --context "$CTX" "$@"; }

# Pinned tools, verified by checksum before use.
KIND_VERSION=v0.33.0
ARGOCD_VERSION=v3.5.4
ARGOCD_COMMIT=d6d5b248ce00e1a2c512068002a93d3319767087 # the $ARGOCD_VERSION tag
ARGOCD_INSTALL_SHA256=1feb02cc7bacf3a379da58b0ca8c6f07d18288cd04d531dabd08816375e5e010

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
verify() { # file expected-sha256
  got=$(sha256 "$1")
  [ "$got" = "$2" ] || { echo "checksum mismatch for $1: got $got, want $2" >&2; exit 1; }
}

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m | sed 's/x86_64/amd64/; s/aarch64/arm64/')
case "$os-$arch" in
  linux-amd64) KIND_SHA256=aee6151561422756b764a4ae28e7f44cda5af5a9eead3cc9985112b1de8d8e0d ;;
  linux-arm64) KIND_SHA256=20022bee6cfcd5086cb7234d218e3454e6090022f2a8f55d1fa7fcf42c3867a2 ;;
  darwin-amd64) KIND_SHA256=5a99f26f57246dc9319dd294803313197a0f34d33c525b3ea8b655db5916ece0 ;;
  darwin-arm64) KIND_SHA256=0c8c7dbe5e23594a198b786c4bc13dacc101fa6196b0cb0b23a1ca44e61f4b4f ;;
  *) echo "unsupported platform $os-$arch" >&2; exit 1 ;;
esac
KIND=${TMPDIR:-/tmp}/kind-$KIND_VERSION-$os-$arch
if [ ! -x "$KIND" ]; then
  curl -sSfLo "$KIND.download" "https://github.com/kubernetes-sigs/kind/releases/download/$KIND_VERSION/kind-$os-$arch"
  verify "$KIND.download" "$KIND_SHA256"
  chmod +x "$KIND.download" && mv "$KIND.download" "$KIND"
fi

if [ "${1:-}" = cleanup ]; then
  "$KIND" delete cluster --name "$CLUSTER"
  exit 0
fi

echo "==> cluster"
"$KIND" get clusters | grep -qx "$CLUSTER" || "$KIND" create cluster --name "$CLUSTER" --wait 120s

echo "==> Argo CD $ARGOCD_VERSION"
k get namespace argocd >/dev/null 2>&1 || k create namespace argocd
install=${TMPDIR:-/tmp}/argocd-install-$ARGOCD_VERSION.yaml
curl -sSfLo "$install" "https://raw.githubusercontent.com/argoproj/argo-cd/$ARGOCD_COMMIT/manifests/install.yaml"
verify "$install" "$ARGOCD_INSTALL_SHA256"
k apply -n argocd --server-side -f "$install" >/dev/null
k -n argocd rollout status deploy/argocd-repo-server --timeout=300s

echo "==> images"
[ -f schemas/kubernetes-1.37.json ] || schemas/fetch.sh 1.37
# The schemas directory holds only this downloaded file, so a fresh checkout has none.
mkdir -p deploy/argocd/e2e/repo/schemas
cp schemas/kubernetes-1.37.json deploy/argocd/e2e/repo/schemas/
docker build -q -f deploy/argocd/Dockerfile -t cleat-cmp:dev .
docker build -q -f deploy/argocd/e2e/git-server.Dockerfile -t cleat-e2e-git:dev deploy/argocd/e2e
"$KIND" load docker-image --name "$CLUSTER" cleat-cmp:dev cleat-e2e-git:dev

echo "==> plugin sidecar and git server"
sed 's#image: cleat-cmp:dev#image: cleat-cmp:dev\n          imagePullPolicy: Never#' deploy/argocd/repo-server-patch.yaml |
  k -n argocd patch deployment argocd-repo-server --patch-file /dev/stdin
k apply -f deploy/argocd/e2e/manifests.yaml
k -n argocd rollout status deploy/argocd-repo-server --timeout=180s
k -n argocd rollout status deploy/git-server --timeout=120s

echo "==> sync"
k apply -f deploy/argocd/e2e/application.yaml
status=""
for _ in $(seq 1 40); do
  status=$(k -n argocd get application cleat-e2e -o jsonpath='{.status.sync.status}/{.status.health.status}' 2>/dev/null || true)
  [ "$status" = Synced/Healthy ] && break
  sleep 6
done
if [ "$status" != Synced/Healthy ]; then
  echo "FAIL: application is $status"
  k -n argocd get application cleat-e2e -o jsonpath='{range .status.conditions[*]}{.type}: {.message}{"\n"}{end}'
  exit 1
fi

echo "==> verify"
fail=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1 = $2"; else echo "FAIL $1 = $2 (want $3)"; fail=1; fi; }
check "ConfigMap data.ENV (parameter env)" "$(k -n cleat-e2e get cm settings -o jsonpath='{.data.ENV}')" prod
check "backend replicas (parameter replicas)" "$(k -n cleat-e2e get deploy backend -o jsonpath='{.spec.replicas}')" 2
check "frontend tier label (with patch)" "$(k -n cleat-e2e get deploy frontend -o jsonpath='{.metadata.labels.tier}')" edge
for r in cm/settings:0 deploy/backend:1 deploy/frontend:2; do
  check "$r sync wave" "$(k -n cleat-e2e get "${r%:*}" -o jsonpath='{.metadata.annotations.argocd\.argoproj\.io/sync-wave}')" "${r#*:}"
done
# Waves are applied in order: each later wave was created after the earlier one.
t0=$(k -n cleat-e2e get cm settings -o jsonpath='{.metadata.creationTimestamp}')
t1=$(k -n cleat-e2e get deploy backend -o jsonpath='{.metadata.creationTimestamp}')
t2=$(k -n cleat-e2e get deploy frontend -o jsonpath='{.metadata.creationTimestamp}')
if [ "$t0" \< "$t1" ] && [ "$t1" \< "$t2" ]; then echo "ok   waves applied in order ($t0 < $t1 < $t2)"; else echo "FAIL wave order $t0 $t1 $t2"; fail=1; fi

echo "==> invalid parameters are reported by Argo"
k -n argocd patch application cleat-e2e --type merge \
  -p '{"spec":{"source":{"plugin":{"parameters":[{"name":"env","string":"production"}]}}}}' >/dev/null
msg=""
for _ in $(seq 1 20); do
  msg=$(k -n argocd get application cleat-e2e -o jsonpath='{.status.conditions[*].message}')
  case "$msg" in *"expected \`Env\`"*) break ;; esac
  sleep 3
done
case "$msg" in
  *"input \`env\`: expected \`Env\`"*) echo "ok   error shown: ${msg##*: error: }" ;;
  *) echo "FAIL no Cleat error in conditions: $msg"; fail=1 ;;
esac

echo "==> imports cannot leave the repository checkout"
k -n argocd patch application cleat-e2e --type merge -p '{"spec":{"source":{"plugin":{
  "env":[{"name":"CLEAT_FILE","value":"escape.cleat"}],
  "parameters":[{"name":"env","string":"prod"}]}}}}' >/dev/null
msg=""
for _ in $(seq 1 20); do
  msg=$(k -n argocd get application cleat-e2e -o jsonpath='{.status.conditions[*].message}')
  case "$msg" in *"outside the project root"*) break ;; esac
  sleep 3
done
case "$msg" in
  *"is outside the project root"*) echo "ok   escape rejected: ${msg##*: error: }" ;;
  *) echo "FAIL escape not rejected: $msg"; fail=1 ;;
esac

[ "$fail" = 0 ] && echo "PASS" || { echo "FAILED"; exit 1; }
