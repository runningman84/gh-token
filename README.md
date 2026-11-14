# gh-token

A small Rust web service that keeps the credentials of a GitHub App **private**
and hands out **short-lived GitHub installation tokens** to in-cluster clients
(Hermes, pi, agents, …).

- The **server** (`gh-token-server`) holds the GitHub App ID, installation ID
  and RSA private key (from a sops-encrypted Kubernetes secret), talks to the
  GitHub API, and exposes a cluster-internal HTTP endpoint.
- The **client** (`gh-token`) is a tiny CLI that asks the server for a token
  and prints it to stdout — so scripts and agents can do
  `GH_TOKEN=$(gh-token --client hermes)`.

GitHub installation tokens always live for exactly one hour. The server adds a
*soft TTL* layer on top: it caches one token per client, renews tokens before
they run out (on demand when a client asks again, and proactively in the
background), and lets clients request shorter effective lifetimes.

```
                    ┌─────────────────────────────┐
 hermes / pi / ...  │  gh-token (CLI client)      │
 ──────────────────▶│  prints token to stdout     │
                    └──────────────┬──────────────┘
                                   │ GET /token?client=hermes
                    ┌──────────────▼──────────────┐        ┌──────────────┐
   in-cluster only  │  gh-token-server (Rust)     │  JWT   │  GitHub API  │
   (Cilium policy)  │  • cache + soft TTLs        │───────▶│  api.github  │
                    │  • rate limiting per client │  RS256 │  .com        │
                    │  • renewal watcher          │        └──────────────┘
                    │  • /metrics /health /ready  │
                    │  • credentials in secret    │
                    └─────────────────────────────┘
```

## Features

- **Server** (Rust, `axum` + `tokio`)
  - `GET/POST /token?client=<id>&refresh=<bool>&ttl=<secs>` — issues or serves
    the cached installation token for a client
  - `GET /health` — liveness
  - `GET /ready` — readiness (200 only while the GitHub API is reachable)
  - `GET /metrics` — Prometheus metrics (requests, latency, issued/renewed/
    cached tokens, GitHub errors, active tokens, rate-limit rejections)
  - Per-client sliding-window rate limiting (429 + `Retry-After`)
  - Background **watcher** renews expiring tokens for active clients
  - Background **probe** for the readiness state
  - **Startup validation**: fails fast (exit 1) when the credentials are
    invalid, and logs the established auth level (app, installation,
    permissions, repository selection, bot login)
  - Tokens are only ever logged in masked form: `ghs_abc1…wxyz (len 86)`
- **Client** (`gh-token` binary)
  - `gh-token [--client NAME] [--refresh] [--ttl SECS]` → token on stdout
  - `gh-token health` → liveness/readiness check
  - Retries transient failures (429/5xx/network) with backoff, honoring
    `Retry-After`
  - Defaults to the in-cluster service URL when `KUBERNETES_SERVICE_HOST` is
    set
- **Helm chart** (published as an OCI artifact)
- **Release pipeline** (Conventional Commits + semantic-release)
  - binaries for linux (amd64/arm64, static musl), darwin (amd64/arm64),
    windows (amd64)
  - multi-arch container image + SBOM (Syft) + vulnerability scan (Grype)
  - Helm chart to `oci://ghcr.io/runningman84/charts/gh-token`

## Server API

### `GET /token`

| Param     | Description                                                                 |
|-----------|-----------------------------------------------------------------------------|
| `client`  | **required.** Identifier, `[a-z0-9_-]{1,64}` (pod names work out of the box) |
| `refresh` | `true` → ignore the cache and force a fresh token from GitHub               |
| `ttl`     | Requested effective TTL in seconds (clamped to `GHTS_MIN_TTL`…`GHTS_TOKEN_TTL`) |

Response:

```json
{
  "token": "ghs_…",
  "client": "hermes",
  "expires_at": "2026-01-01T00:59:59Z",
  "ttl": 3540,
  "github_expires_at": "2026-01-01T01:00:00Z",
  "cached": false,
  "renewed": true
}
```

`expires_at`/`ttl` are the *server-enforced* lifetime (possibly shorter than
the underlying GitHub token, which always expires after one hour — see
`github_expires_at`).

### Authentication (optional)

When `GHTS_API_KEY` is set, requests must present the key as
`X-GH-Token-Key: <key>` or `Authorization: Bearer <key>`.

## Configuration (environment)

| Variable                  | Default                    | Description |
|---------------------------|----------------------------|-------------|
| `GH_APP_ID`               | — (required)               | GitHub App ID |
| `GH_APP_INSTALLATION_ID`  | — (required)               | GitHub App installation ID |
| `GH_APP_KEY_B64`          | — (required)               | RSA private key, base64-encoded PEM (raw PEM also accepted) |
| `GHTS_BIND`               | `0.0.0.0:8080`             | Listen address |
| `GHTS_API_KEY`            | *(unset = no auth)*        | Optional shared API key |
| `GHTS_TOKEN_TTL`          | `3600`                     | Default soft TTL in seconds (max 3600) |
| `GHTS_MIN_TTL`            | `60`                       | Smallest requestable soft TTL |
| `GHTS_RENEW_BEFORE`       | `300`                      | Renew when less than this much (soft) lifetime remains |
| `GHTS_ACTIVITY_WINDOW`    | `900`                      | A client is "active" while it has requested a token within this window |
| `GHTS_WATCH_INTERVAL`     | `30`                       | Watcher poll interval (seconds) |
| `GHTS_PROBE_INTERVAL`     | `30`                       | GitHub reachability probe interval (seconds) |
| `GHTS_READY_MAX_AGE`      | `90`                       | `/ready` is ready only if the last probe succeeded within this age |
| `GHTS_RATE_MAX`           | `30`                       | Max token requests per client per window |
| `GHTS_RATE_WINDOW`        | `60`                       | Rate-limit window (seconds) |
| `GHTS_GITHUB_BASE_URL`    | `https://api.github.com`   | GitHub API base URL (GHE / tests) |
| `GHTS_LOG` / `RUST_LOG`   | `info`                     | Log level / filter |
| `GHTS_LOG_JSON`           | `false`                    | JSON log format |

## GitHub App setup

1. Create a GitHub App ([developer settings](https://github.com/settings/apps))
   with the permissions you want agents to have (e.g. `contents: read/write`).
2. Install the app on the user/org/repo(s) that should be accessible.
3. Generate an RSA private key and store it:

   ```sh
   openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out app.key.pem
   base64 -w0 app.key.pem   # -> GH_APP_KEY_B64
   ```

4. Note the **App ID** (on the app's main page) and the **Installation ID**
   (in the URL when editing the installation).

## Kubernetes deployment

### Helm (recommended)

```sh
helm repo add gh-token oci://ghcr.io/runningman84/charts/gh-token
helm install gh-token gh-token/gh-token -n gh-token --create-namespace \
  --set secret.data.GH_APP_ID=1234567 \
  --set secret.data.GH_APP_INSTALLATION_ID=7654321 \
  --set secret.data.GH_APP_KEY_B64=<base64-pem> \
  --set cilium.enabled=true
```

Options of interest:

- `secret.create=false` + `secret.name=<existing>` — use a secret you manage
  (e.g. sops + external-secrets).
- `secret.apiKey=<key>` — enable the optional API key.
- `cilium.enabled=true` + `cilium.clients=[{namespace, labels}]` — restrict
  who may call the server and pin egress to `api.github.com:443`.
- `serviceMonitor.enabled=true` — expose `/metrics` to prometheus-operator.

The server runs in the `gh-token` namespace as a ClusterIP service at
`gh-token.gh-token.svc.cluster.local:8080`.

### Plain manifests

See [`deploy/`](deploy/) for the raw Kubernetes manifests (namespace,
deployment, service, ServiceAccount, sops-encrypted secret example, Cilium
network policy).

Encrypt the secret with sops:

```sh
sops --encrypt --encrypted-regex 'stringData' deploy/secret.example.yaml \
  > deploy/secret.yaml
kubectl apply -f deploy/secret.yaml
```

## Client usage

```sh
# Inside the cluster (defaults to the in-cluster service URL):
gh-token --client hermes

# Anywhere:
gh-token --url http://gh-token.gh-token.svc.cluster.local:8080 \
  --client hermes --ttl 900

# Force a fresh token:
gh-token --client pi --refresh

# Check the server:
gh-token health

# With an API key:
gh-token --client pi --api-key <key>
```

Environment: `GHTS_URL`, `GHTS_CLIENT`, `GHTS_API_KEY` are honored.

Typical agent wiring:

```sh
export GH_TOKEN="$(gh-token --client hermes)"
gh pr view 42
```

## Prometheus metrics

| Metric | Type | Description |
|--------|------|-------------|
| `ghts_token_requests_total{client,status}` | counter | Token requests |
| `ghts_token_request_duration_seconds{client}` | histogram | Token request latency |
| `ghts_tokens_issued_total` | counter | Tokens created from GitHub |
| `ghts_tokens_renewed_total` | counter | Renewals (fresh token for an existing client) |
| `ghts_tokens_served_cached_total` | counter | Requests served from cache |
| `ghts_tokens_expired_total` | counter | Expired tokens |
| `ghts_github_api_errors_total{operation}` | counter | GitHub API errors |
| `ghts_active_tokens` | gauge | Tracked tokens in the cache |
| `ghts_token_lifetime_seconds` | histogram | Lifetime of issued tokens |
| `ghts_rate_limit_rejections_total` | counter | Rate-limit rejections |
| `ghts_github_reachable` | gauge | Last probe outcome (1/0) |

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The test suite includes a **mock GitHub API server** (`testutil/`) that
verifies the app JWT signatures (RS256 against a generated test keypair),
simulates `GET /app`, `GET /app/installations/{id}`,
`POST /app/installations/{id}/access_tokens`, `GET /user` and
`GET /rate_limit`, and can be toggled to fail per endpoint. Integration tests
cover the token flow (issue/cache/refresh/rate-limit), the renewal watcher,
readiness flapping, API-key enforcement, startup validation, and the client
end-to-end (binary against the real server + mock GitHub).

Releases: follow [Conventional Commits](https://www.conventionalcommits.org/)
(`feat:`, `fix:`, `perf:`, …; `!` or `BREAKING CHANGE:` for breaking
changes). Pushing to `main` runs the test suite, then semantic-release bumps
the workspace + chart + `package.json` version, tags `vX.Y.Z`, publishes a
GitHub release with binaries, SBOM/scan results and the Helm chart, and pushes
the container image.

## License

MIT
