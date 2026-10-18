#!/usr/bin/env bash
# Checks the rendered chart for contracts that `helm lint` does not see.
# Needs helm and yq (mikefarah, v4).
set -euo pipefail
cd "$(dirname "$0")/../../.."
chart=deploy/helm/lastmile
HELM=${HELM:-helm}
render() {
  $HELM template lastmile "$chart" --set oidc.issuer=https://idp.example.com "$@"
}
fail() { echo "FAIL: $*" >&2; exit 1; }

# An exposed UI Service never carries the unauthenticated metrics port.
out=$(render --set service.type=LoadBalancer --set serviceMonitor.enabled=true)
ui=$(yq 'select(.kind == "Service" and .metadata.name == "lastmile")' <<<"$out")
[[ $(yq '.spec.type' <<<"$ui") == LoadBalancer ]] || fail "UI Service type not applied"
[[ $(yq '[.spec.ports[].name] | join(",")' <<<"$ui") == http ]] || fail "UI Service exposes more than http"
metrics=$(yq 'select(.kind == "Service" and .metadata.name == "lastmile-metrics")' <<<"$out")
[[ $(yq '.spec.type' <<<"$metrics") == ClusterIP ]] || fail "metrics Service is not ClusterIP"
[[ $(yq '.spec.ports[0].name' <<<"$metrics") == metrics ]] || fail "metrics Service has no metrics port"
sm=$(yq 'select(.kind == "ServiceMonitor")' <<<"$out")
[[ $(yq '.spec.selector.matchLabels."app.kubernetes.io/component"' <<<"$sm") == metrics ]] \
  || fail "ServiceMonitor does not select the metrics Service"

# The database is on the chart's PVC, or on an existing one (then no PVC is
# created), and one pod at a time opens it.
deploy=$(yq 'select(.kind == "Deployment")' <<<"$out")
[[ $(yq '.spec.strategy.type' <<<"$deploy") == Recreate ]] || fail "Deployment strategy is not Recreate"
[[ $(yq '.spec.replicas' <<<"$deploy") == 1 ]] || fail "more than one replica"
[[ $(yq '.spec.template.spec.volumes[] | select(.name == "data") | .persistentVolumeClaim.claimName' <<<"$deploy") == lastmile-data ]] \
  || fail "data volume is not the chart's PVC"
[[ $(yq 'select(.kind == "PersistentVolumeClaim") | .metadata.name' <<<"$out") == lastmile-data ]] || fail "no PVC rendered"
existing=$(render --set persistence.existingClaim=parcels)
[[ -z $(yq 'select(.kind == "PersistentVolumeClaim") | .metadata.name' <<<"$existing") ]] || fail "PVC rendered with existingClaim"
[[ $(yq 'select(.kind == "Deployment") | .spec.template.spec.volumes[] | select(.name == "data") | .persistentVolumeClaim.claimName' <<<"$existing") == parcels ]] \
  || fail "existingClaim not mounted"

# The default image tag is the chart's appVersion, which the release
# workflow publishes (see .github/workflows/release.yml).
app=$(yq '.appVersion' "$chart/Chart.yaml")
image=$(yq '.spec.template.spec.containers[0].image' <<<"$deploy")
[[ $image == *":$app" ]] || fail "default image $image is not tagged $app"
cargo_version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[[ $app == "$cargo_version" ]] || fail "Chart appVersion $app differs from Cargo.toml version $cargo_version"

echo "chart render checks passed"
