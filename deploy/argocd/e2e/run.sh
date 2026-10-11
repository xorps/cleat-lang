#!/bin/sh
# End-to-end test of the Cleat Argo CD plugin on a throwaway kind cluster.
#
#   deploy/argocd/e2e/run.sh            # create cluster, install, sync, verify
#   deploy/argocd/e2e/run.sh cleanup    # delete the cluster
#
# Needs docker and kubectl. Downloads kind if it isn't on PATH.
set -eu
cd "$(dirname "$0")/../../.."
CLUSTER=cleat-e2e
CTX=kind-$CLUSTER
k() { kubectl --context "$CTX" "$@"; }

KIND=$(command -v kind || true)
if [ -z "$KIND" ]; then
  KIND=${TMPDIR:-/tmp}/kind-cleat
  if [ ! -x "$KIND" ]; then
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m | sed 's/x86_64/amd64/; s/aarch64/arm64/')
    curl -sSfLo "$KIND" "https://kind.sigs.k8s.io/dl/v0.29.0/kind-$os-$arch"
    chmod +x "$KIND"
  fi
fi

if [ "${1:-}" = cleanup ]; then
  "$KIND" delete cluster --name "$CLUSTER"
  exit 0
fi

echo "==> cluster"
"$KIND" get clusters | grep -qx "$CLUSTER" || "$KIND" create cluster --name "$CLUSTER" --wait 120s

echo "==> Argo CD"
k get namespace argocd >/dev/null 2>&1 || k create namespace argocd
k apply -n argocd --server-side -f https://raw.githubusercontent.com/argoproj/argo-cd/stable/manifests/install.yaml >/dev/null
k -n argocd rollout status deploy/argocd-repo-server --timeout=300s

echo "==> images"
[ -f schemas/kubernetes-1.31.json ] || schemas/fetch.sh 1.31
cp schemas/kubernetes-1.31.json deploy/argocd/e2e/repo/schemas/
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
