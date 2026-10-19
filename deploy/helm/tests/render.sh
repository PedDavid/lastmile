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

# The model that drafts rules is off by default, and its key is mounted
# from a Secret as a file when set.
env=$(yq '.spec.template.spec.containers[0].env[].name' <<<"$deploy")
grep -q LASTMILE_LLM_MODEL <<<"$env" && fail "the model is on by default"
llm=$(render --set llm.model=gpt-x --set llm.apiKey.existingSecret=llm-key | yq 'select(.kind == "Deployment")')
[[ $(yq '.spec.template.spec.containers[0].env[] | select(.name == "LASTMILE_LLM_API_KEY_FILE") | .value' <<<"$llm") == /etc/lastmile/llm/api-key ]] \
  || fail "the LLM key file is not set"
[[ $(yq '.spec.template.spec.volumes[] | select(.name == "llm") | .secret.secretName' <<<"$llm") == llm-key ]] \
  || fail "the LLM key Secret is not mounted"
nokey=$(render --set llm.model=local --set llm.baseUrl=http://ollama:11434/v1 | yq 'select(.kind == "Deployment")')
[[ -z $(yq '.spec.template.spec.volumes[] | select(.name == "llm")' <<<"$nokey") ]] || fail "a Secret is mounted without a key"

# The analyzer is off by default; enabling it starts a cluster-internal
# Service the app is pointed at, which the app's own Service does not select.
grep -q LASTMILE_PRESIDIO_URL <<<"$env" && fail "the analyzer is on by default"
[[ -z $(yq 'select(.metadata.name == "lastmile-presidio")' <<<"$out") ]] || fail "the analyzer is rendered by default"
pres=$(render --set presidio.enabled=true)
[[ $(yq 'select(.kind == "Deployment" and .metadata.name == "lastmile") | .spec.template.spec.containers[0].env[] | select(.name == "LASTMILE_PRESIDIO_URL") | .value' <<<"$pres") == http://lastmile-presidio.default.svc:3000 ]] \
  || fail "the app is not pointed at the analyzer it starts"
[[ $(yq 'select(.kind == "Service" and .metadata.name == "lastmile-presidio") | .spec.type' <<<"$pres") == ClusterIP ]] \
  || fail "the analyzer Service is not ClusterIP"
app_selector=$(yq 'select(.kind == "Service" and .metadata.name == "lastmile") | .spec.selector."app.kubernetes.io/name"' <<<"$pres")
pod_name=$(yq 'select(.kind == "Deployment" and .metadata.name == "lastmile-presidio") | .spec.template.metadata.labels."app.kubernetes.io/name"' <<<"$pres")
[[ -n $pod_name && $pod_name != "$app_selector" ]] || fail "the app's Service would select the analyzer pod"
[[ $(yq 'select(.kind == "Deployment" and .metadata.name == "lastmile-presidio") | .spec.template.spec.containers[0].securityContext.readOnlyRootFilesystem' <<<"$pres") == true ]] \
  || fail "the analyzer's filesystem is writable"
ext=$(render --set presidio.url=http://presidio.tools:3000 | yq 'select(.kind == "Deployment" and .metadata.name == "lastmile")')
[[ $(yq '.spec.template.spec.containers[0].env[] | select(.name == "LASTMILE_PRESIDIO_URL") | .value' <<<"$ext") == http://presidio.tools:3000 ]] \
  || fail "an existing analyzer's URL is not used"
[[ -z $(render --set presidio.url=http://presidio.tools:3000 | yq 'select(.metadata.name == "lastmile-presidio")') ]] \
  || fail "an analyzer is started although one is given"

# Courier tracking is off by default; enabling the browser starts a
# cluster-internal Chromium the app is pointed at, locked down and apart from
# the app's own Service.
grep -q LASTMILE_BROWSER_URL <<<"$env" && fail "the browser is on by default"
[[ -z $(yq 'select(.metadata.name == "lastmile-chromium")' <<<"$out") ]] || fail "the browser is rendered by default"
br=$(render --set browser.enabled=true)
[[ $(yq 'select(.kind == "Deployment" and .metadata.name == "lastmile") | .spec.template.spec.containers[0].env[] | select(.name == "LASTMILE_BROWSER_URL") | .value' <<<"$br") == http://lastmile-chromium.default.svc:9222 ]] \
  || fail "the app is not pointed at the browser it starts"
[[ $(yq 'select(.kind == "Service" and .metadata.name == "lastmile-chromium") | .spec.type' <<<"$br") == ClusterIP ]] \
  || fail "the browser Service is not ClusterIP"
br_name=$(yq 'select(.kind == "Deployment" and .metadata.name == "lastmile-chromium") | .spec.template.metadata.labels."app.kubernetes.io/name"' <<<"$br")
br_app=$(yq 'select(.kind == "Service" and .metadata.name == "lastmile") | .spec.selector."app.kubernetes.io/name"' <<<"$br")
[[ -n $br_name && $br_name != "$br_app" ]] || fail "the app's Service would select the browser pod"
chromium=$(yq 'select(.kind == "Deployment" and .metadata.name == "lastmile-chromium") | .spec.template.spec.containers[0]' <<<"$br")
[[ $(yq '.securityContext.readOnlyRootFilesystem' <<<"$chromium") == true ]] || fail "the browser's filesystem is writable"
[[ $(yq '.securityContext.capabilities.drop[0]' <<<"$chromium") == ALL ]] || fail "the browser keeps capabilities"
[[ $(yq 'select(.kind == "Deployment" and .metadata.name == "lastmile-chromium") | .spec.template.spec.securityContext.runAsNonRoot' <<<"$br") == true ]] \
  || fail "the browser runs as root"
brext=$(render --set browser.url=http://chromium.tools:9222)
[[ $(yq 'select(.kind == "Deployment" and .metadata.name == "lastmile") | .spec.template.spec.containers[0].env[] | select(.name == "LASTMILE_BROWSER_URL") | .value' <<<"$brext") == http://chromium.tools:9222 ]] \
  || fail "an existing browser's URL is not used"
[[ -z $(yq 'select(.metadata.name == "lastmile-chromium")' <<<"$brext") ]] || fail "a browser is started although one is given"

# The default image tag is the chart's appVersion, which the release
# workflow publishes (see .github/workflows/release.yml).
app=$(yq '.appVersion' "$chart/Chart.yaml")
image=$(yq '.spec.template.spec.containers[0].image' <<<"$deploy")
[[ $image == *":$app" ]] || fail "default image $image is not tagged $app"
cargo_version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[[ $app == "$cargo_version" ]] || fail "Chart appVersion $app differs from Cargo.toml version $cargo_version"

echo "chart render checks passed"
