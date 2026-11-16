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
  - `GET/POST /token?client=<id>&refresh=<bool>&ttl=<secs>&purpose=<text>` —
    issues or serves the cached installation token for a client
  - `GET /health` — liveness
  - `GET /ready` — readiness: 200 only if the startup credential check
    passed **and** the last GitHub reachability probe succeeded
    (otherwise 503, with `startup_check` / `startup_error` in the body)
  - `GET /permissions` — app-declared and installation-effective GitHub
    permissions (503 until startup validation succeeds)
  - `GET /metrics` — Prometheus metrics (requests, latency, issued/renewed/
    cached tokens, GitHub errors, active tokens, rate-limit rejections,
    GitHub rate-limit responses, cooldown, plus standard unprefixed
    `process_*` metrics: memory, CPU time, threads, open fds, uptime)
  - Per-client sliding-window rate limiting (429 + `Retry-After`)
  - **GitHub rate-limit awareness**: detects 429 / 403 rate-limit responses
    (primary limit, secondary/abuse limits), backs off exponentially, and
    suppresses further GitHub calls for the cooldown period. While GitHub is
    rate-limited, `/token` answers `503` + `Retry-After` (clients retry)
    instead of hammering the API
  - Background **watcher** renews expiring tokens for active clients
  - Background **probe** for the readiness state. The probe calls
    `GET /rate_limit` with a cached installation token, so it uses the
    installation's rate-limit budget instead of the shared 60/h
    unauthenticated per-IP budget (and also verifies the credentials keep
    working)
  - **Startup validation**: validates the GitHub App credentials at boot
    and logs the established auth level (app, installation, permissions,
    repository selection, bot login). Until it passes, `/ready` returns
    503 (`startup_check: "failed"`); a failed check is retried in the
    background with exponential backoff (30s .. 300s), so a transient
    outage at boot recovers without a pod restart
  - **Logs every GitHub request/response** (method, path, auth kind, status,
    duration, truncated body)
  - Tokens are only ever logged in masked form: `ghs_abc1…wxyz (len 86)` —
    the request logger also scrubs any `ghs_`/`ghp_`/… token that appears in
    a response body
- **Client** (`gh-token` binary)
  - `gh-token [--client NAME] [--refresh] [--ttl SECS] [--purpose TEXT]` → token on stdout
  - `gh-token health` → liveness/readiness check
  - `gh-token permissions` → pretty-printed JSON with app and installation permissions
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

### `GET /permissions`

Returns the permissions declared by the GitHub App (`app_permissions`) and
those effectively granted to this installation (`installation_permissions`),
along with app/installation IDs and repository selection. These values come
from GitHub's `GET /app` and `GET /app/installations/{installation_id}` APIs.
The endpoint returns 503 until startup credential validation succeeds. Run
`gh-token permissions` to display the report as formatted JSON.

### `GET /token`

| Param     | Description                                                                 |
|-----------|-----------------------------------------------------------------------------|
| `client`  | **required.** Identifier, `[a-z0-9_-]{1,64}` (pod names work out of the box) |
| `refresh` | `true` → ignore the cache and force a fresh token from GitHub               |
| `ttl`     | Requested effective TTL in seconds (clamped to `GHTS_MIN_TTL`…`GHTS_TOKEN_TTL`) |
| `purpose` | Optional free-form audit metadata, 1–256 chars, no control characters (e.g. `triage of open issues in project abc`). Returned in the response, included in issue/renewal logs. By default purely advisory: the cached token is served regardless of purpose and no extra GitHub token is created. With `GHTS_PURPOSE_KEY=true` the purpose becomes part of the cache key and a differing purpose issues a fresh token. |

Response:

```json
{
  "token": "ghs_…",
  "client": "hermes",
  "expires_at": "2026-01-01T00:59:59Z",
  "ttl": 3540,
  "github_expires_at": "2026-01-01T01:00:00Z",
  "cached": false,
  "renewed": true,
  "purpose": "triage of open issues in project abc"
}
```

`expires_at`/`ttl` are the *server-enforced* lifetime (possibly shorter than
the underlying GitHub token, which always expires after one hour — see
`github_expires_at`). `purpose` is omitted when not requested.

### Authentication (optional)

When `GHTS_API_KEY` is set, requests must present the key as
`X-GH-Token-Key: <key>` or `Authorization: Bearer <key>`.

#### Per-client keys

`GHTS_CLIENT_KEYS` is a JSON object mapping client ids to keys:

```sh
GHTS_CLIENT_KEYS='{"hermes":"hermes-key","pi":"pi-key"}'
```

A request for a client **present in the map** must send the matching key via
the same `X-GH-Token-Key` / `Authorization: Bearer` header (401
`missing`/`invalid API key for this client` otherwise). Clients absent from
the map are unaffected, and `GHTS_API_KEY` (when set) still applies on top.
The client binary sends `--api-key` for both checks.

## Configuration (environment)

| Variable                  | Default                    | Description |
|---------------------------|----------------------------|-------------|
| `GH_APP_ID`               | — (required)               | GitHub App ID |
| `GH_APP_INSTALLATION_ID`  | — (required)               | GitHub App installation ID |
| `GH_APP_KEY_B64`          | — (required)               | RSA private key, base64-encoded PEM (raw PEM also accepted) |
| `GHTS_BIND`               | `0.0.0.0:8080`             | Listen address |
| `GHTS_API_KEY`            | *(unset = no auth)*        | Optional shared API key |
| `GHTS_CLIENT_KEYS`        | *(unset = off)*            | JSON object of per-client keys, e.g. `{"hermes":"…"}`; only listed clients are gated |
| `GHTS_PURPOSE_KEY`        | `false`                    | When true, the token purpose is part of the cache key (differing purpose ⇒ fresh token); off by default (advisory only) |
| `GHTS_TOKEN_TTL`          | `3600`                     | Default soft TTL in seconds (max 3600) |
| `GHTS_MIN_TTL`            | `60`                       | Smallest requestable soft TTL |
| `GHTS_RENEW_BEFORE`       | `300`                      | Renew when less than this much (soft) lifetime remains |
| `GHTS_ACTIVITY_WINDOW`    | `900`                      | A client is "active" while it has requested a token within this window |
| `GHTS_WATCH_INTERVAL`     | `30`                       | Watcher poll interval (seconds) |
| `GHTS_PROBE_INTERVAL`     | `30`                       | GitHub reachability probe interval (seconds) |
| `GHTS_READY_MAX_AGE`      | `90`                       | `/ready` is ready only if the last probe succeeded within this age |
| `GHTS_RATE_MAX`           | `30`                       | Max token requests per client per window |
| `GHTS_RATE_WINDOW`        | `60`                       | Rate-limit window (seconds) |
| `GHTS_BACKOFF_BASE_SECS`  | `2`                        | Base delay for the GitHub rate-limit backoff (doubles per consecutive limit, capped at `GHTS_BACKOFF_MAX_SECS`) |
| `GHTS_BACKOFF_MAX_SECS`   | `300`                      | Maximum GitHub rate-limit cooldown (seconds) |
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
  who may call the server and pin egress to `github.com:443` and the configured
  GitHub API FQDN (by default, `api.github.com:443`). DNS queries are restricted
  to cluster DNS and inspected by Cilium for FQDN policy enforcement.
- `serviceMonitor.enabled=true` — expose `/metrics` to prometheus-operator.
- `prometheusRules.enabled=true` — install a `PrometheusRule` with alerting
  rules for GitHub reachability, startup validation, sustained rate-limit
  backoff, GitHub API failures, token-endpoint 5xx/latency and client
  throttling. The rules live in `values.yaml` (`prometheusRules.rules`), so
  thresholds and messages can be customized in your values file.

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

# Attach a purpose (audit metadata) and, when required, the client key:
gh-token --client hermes --api-key "***" \
  --purpose "triage of open issues in project abc"

# Check the server:
gh-token health

# With an API key:
gh-token --client pi --api-key <key>
```

Environment: `GHTS_URL`, `GHTS_CLIENT`, `GHTS_API_KEY`, `GHTS_PURPOSE` are
honored.

### Using with the `gh` CLI

The GitHub CLI authenticates with the `GH_TOKEN` environment variable, and
this client prints just the token to stdout (logs go to stderr), so they
compose directly. The token belongs to the GitHub App's installation, so `gh`
acts as the bot identity — `gh auth status` will show the installation, not
your user account.

One-off:

```sh
GH_TOKEN="$(gh-token --client hermes)" gh pr view 42
```

For repeated use, define a wrapper so every `gh` invocation gets a fresh (or
cached) token without exporting it to the whole shell:

```sh
gh() {
  GH_TOKEN="$(gh-token --client "${GHTS_CLIENT:-hermes}")" \
    command gh "$@"
}
```

The wrapper inherits the client's environment, so you specify the token URL
and client without editing it:

```sh
# Inside the cluster: in-cluster URL is the default.
gh pr list

# Outside the cluster (or against another server):
GHTS_URL=http://gh-token.gh-token.svc.cluster.local:8080 gh pr list
GHTS_URL=http://127.0.0.1:18080 GHTS_CLIENT=pi gh pr list
```

If the client needs an API key or a purpose, set `GHTS_API_KEY` / `GHTS_PURPOSE`
in the same way — they are passed through to `gh-token` untouched.

Note: installation tokens are `integration`-type, which GitHub does not grant
access to the user-scoped `GET /user` endpoint. All normal repo, PR, issue,
and API operations work, but `gh auth status` (and anything that calls
`/user`) will return 403 — that is expected, not a broken token.

## Prometheus metrics

Every `ghts_*` metric carries the `app_id` and `install_id` labels (the
GitHub App and installation this instance serves). These let you select and
group metrics for different app installations in the same Prometheus backend;
they do not distinguish replicas serving the same installation. Label order
in the exposition is not significant and may be sorted by the exporter. The
table below lists only labels in addition to `app_id` and `install_id`.

| Metric | Type | Additional labels | Description |
|--------|------|-------------------|-------------|
| `ghts_token_requests_total` | counter | `client,status` | Token requests |
| `ghts_token_request_duration_seconds` | histogram | `client` | Token request latency |
| `ghts_tokens_issued_total` | counter | — | Tokens created from GitHub |
| `ghts_tokens_renewed_total` | counter | — | Renewals (fresh token for an existing client) |
| `ghts_tokens_served_cached_total` | counter | — | Requests served from cache |
| `ghts_tokens_expired_total` | counter | — | Expired tokens |
| `ghts_github_api_errors_total` | counter | `operation` | GitHub API failures (transport errors, unexpected HTTP responses; rate limits are not counted here) |
| `ghts_github_requests_total` | counter | `path,status` | All requests to the GitHub API by path (real ids kept) and status (HTTP code, or `err` for transport failures) |
| `ghts_github_request_duration_seconds` | histogram | `path` | Duration of requests to the GitHub API |
| `ghts_github_rate_limited_total` | counter | `operation` | GitHub rate-limit responses (429/403) |
| `ghts_github_rate_limit_limit` | gauge | `resource` | Maximum GitHub API requests for a rate-limit resource |
| `ghts_github_rate_limit_used` | gauge | `resource` | GitHub API requests used for a rate-limit resource |
| `ghts_github_rate_limit_remaining` | gauge | `resource` | GitHub API requests remaining for a rate-limit resource |
| `ghts_github_rate_limit_reset_timestamp` | gauge | `resource` | Unix timestamp when a resource's GitHub API rate limit resets |
| `ghts_github_cooldown_seconds` | gauge | — | Remaining seconds of the GitHub rate-limit cooldown |
| `ghts_active_tokens` | gauge | — | Tracked tokens in the cache |
| `ghts_token_lifetime_seconds` | histogram | — | Lifetime of issued tokens |
| `ghts_rate_limit_rejections_total` | counter | — | Rate-limit rejections |
| `ghts_github_reachable` | gauge | — | Last probe outcome (1/0) |
| `ghts_startup_check_ok` | gauge | — | 1 if the startup credential validation passed |
| `ghts_build_info` | gauge | `version` | Build information (1 for the running version) |

Example scrape line:

```text
# HELP ghts_github_requests_total Requests made to the GitHub API by path and status. status is the HTTP code, or "err" for transport failures.
# TYPE ghts_github_requests_total counter
ghts_github_requests_total{app_id="1234567",install_id="7654321",path="/app/installations/7654321/access_tokens",status="201"} 3
```

Counters/histograms only appear once the corresponding label combination has
occurred at least once (an absent series means zero — standard Prometheus
semantics). Histogram `_bucket` series also have Prometheus's generated `le`
label; `_sum` and `_count` use the labels shown above. The always-present
state gauges (`ghts_github_reachable`, `ghts_startup_check_ok`,
`ghts_github_cooldown_seconds`, `ghts_active_tokens`) are pre-initialized at
startup so they are scrapeable immediately.

Standard process metrics (names follow the Prometheus process-collector
convention, no prefix — standard dashboards/alerts apply unchanged):

| Metric | Type | Description |
|--------|------|-------------|
| `process_resident_memory_bytes` | gauge | Process resident memory (RSS) |
| `process_resident_memory_peak_bytes` | gauge | Process peak RSS |
| `process_virtual_memory_bytes` | gauge | Process virtual memory size |
| `process_cpu_seconds_total` | counter | Process CPU time (user + system) |
| `process_num_threads` | gauge | Number of process threads |
| `process_open_fds` | gauge | Number of open file descriptors |
| `process_start_time_seconds` | gauge | Unix timestamp of process start |
| `process_uptime_seconds` | gauge | Seconds since process start |

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The test suite includes a **mock GitHub API server** (`testutil/`) that
verifies the app JWT signatures (RS256 against a generated test keypair),
simulates `GET /app`, `GET /app/installations/{id}`,
`POST /app/installations/{id}/access_tokens`, `GET /installation/repositories`,
`GET /users/{login}` and `GET /rate_limit`, and can be toggled to fail per
endpoint or to answer with rate-limit responses. Integration tests
cover the token flow (issue/cache/refresh/rate-limit), the renewal watcher,
readiness flapping, API-key and per-client key enforcement, purpose handling
(advisory by default and `GHTS_PURPOSE_KEY` isolation), startup validation,
and the client end-to-end (binary against the real server + mock GitHub).

Releases: follow [Conventional Commits](https://www.conventionalcommits.org/)
(`feat:`, `fix:`, `perf:`, …; `!` or `BREAKING CHANGE:` for breaking
changes). Pushing to `main` runs the test suite, then semantic-release bumps
the workspace + chart + `package.json` version, tags `vX.Y.Z`, publishes a
GitHub release with binaries, SBOM/scan results and the Helm chart, and pushes
the container image.

## License

MIT
