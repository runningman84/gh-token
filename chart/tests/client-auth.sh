#!/usr/bin/env bash
# Run from any directory; requires Helm 3+.
set -euo pipefail
chart=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

render() {
  helm template test "$chart" --set secret.create=false --set secret.name=github-app \
    --set networkPolicy.enabled=false --show-only templates/deployment.yaml "$@"
}

render > "$tmp/default"
if grep -q 'name: client-keys' "$tmp/default"; then
  echo 'disabled auth unexpectedly mounted keys' >&2
  exit 1
fi

args=(--set clientAuth.enabled=true
  --set clientAuth.clients[0].name=agent-a
  --set clientAuth.clients[0].secretRef.name=agent-a-auth
  --set clientAuth.clients[0].secretRef.key=api-key
  --set clientAuth.clients[1].name=agent-b
  --set clientAuth.clients[1].secretRef.name=agent-b-auth
  --set clientAuth.clients[1].secretRef.key=api-key)
render "${args[@]}" > "$tmp/enabled"
for expected in 'name: GHTS_CLIENT_KEYS_DIR' 'value: "true"' \
  'mountPath: /etc/gh-token/client-keys' 'readOnly: true' \
  'name: "agent-a-auth"' 'path: "agent-a"' \
  'name: "agent-b-auth"' 'path: "agent-b"' 'optional: false'; do
  grep -Fq "$expected" "$tmp/enabled"
done
if grep -q 'subPath:' "$tmp/enabled"; then
  echo 'must not use subPath for projected keys' >&2
  exit 1
fi

reject() {
  if render "$@" > "$tmp/rejected" 2>&1; then
    echo "expected render to reject: $*" >&2
    exit 1
  fi
}
reject --set clientAuth.enabled=true
reject "${args[@]}" --set clientAuth.requireKnownClient=false
reject "${args[@]}" --set clientAuth.clients[1].name=agent-a
reject "${args[@]}" --set clientAuth.clients[0].name=../agent-a
reject "${args[@]}" --set clientAuth.clients[0].secretRef.key=
reject "${args[@]}" --set secret.apiKey=shared
for key in GHTS_API_KEY GHTS_CLIENT_KEYS GHTS_CLIENT_KEYS_DIR GHTS_REQUIRE_KNOWN_CLIENT; do
  reject "${args[@]}" --set "env.$key=conflict"
  reject "${args[@]}" --set "secret.data.$key=conflict"
done
echo 'clientAuth chart tests passed'
