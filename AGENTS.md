# AGENTS.md — gh-token Development Guide

This file provides guidance for agents (human and AI) working on the **gh-token** project.

## Project Overview

**gh-token** is a Rust workspace with four crates:

| Crate | Path | Purpose |
|-------|------|---------|
| `gh-token-common` | `common/` | Shared types, validation, token masking |
| `gh-token-server` | `server/` | Axum HTTP server that issues GitHub App installation tokens |
| `gh-token-client` | `client/` | CLI binary (`gh-token`) that fetches tokens from the server |
| `gh-token-testutil` | `testutil/` | Mock GitHub API server for integration tests |

The server holds GitHub App credentials and exposes `/token`, `/health`, `/ready`, `/metrics`.
The client is a tiny CLI that prints tokens to stdout (for scriptability).

## Architecture

```
Client (gh-token) ──GET /token──▶ Server (gh-token-server) ──JWT + REST──▶ GitHub API
                                     │
                                     ├── in-memory token cache (soft TTL)
                                     ├── background renewal watcher
                                     ├── background GitHub reachability probe
                                     ├── per-client rate limiting
                                     └── GitHub rate-limit awareness (backoff/cooldown)
```

Key files:
- `server/src/http.rs` — token endpoint, auth, rate limiting, logging
- `server/src/app.rs` — server state, startup validation, background loops
- `server/src/github.rs` — GitHub API client (JWT, token creation, probe)
- `client/src/lib.rs` — CLI logic, retries, user-agent
- `common/src/lib.rs` — shared types, validation, masking
- `testutil/src/lib.rs` — mock GitHub server (verifies RS256 JWT signatures)

## Development Commands

```sh
# Format
cargo fmt --all

# Lint (strict)
cargo clippy --workspace --all-targets -- -D warnings

# Test (includes integration tests against mock GitHub)
cargo test --workspace

# Build release binaries
cargo build --release --workspace
```

## Pre-commit Requirement — **Must Pass Before Declaring Ready**

**Before telling the user a change is ready**, you **must** run the pre-commit hooks
and confirm they pass:

```sh
pre-commit run --all-files
```

This runs:
- `cargo fmt --check` — formatting
- `cargo clippy --workspace --all-targets -- -D warnings` — strict linting
- `cargo check --workspace` — compilation check
- `cargo test --workspace --lib --bins` — unit tests (fast)
- `ruff` — linting for TOML/JSON/YAML/MD
- `pre-commit-hooks` — trailing whitespace, EOF, YAML/JSON/TOML syntax, merge conflicts, line endings

Only after **all hooks pass** should you report the change as complete. This mirrors
the CI gate and prevents "works on my machine" failures.

> **Note**: Integration tests (`cargo test --workspace --tests`) run in CI but are
> intentionally excluded from pre-commit for speed. The fast unit tests (`--lib --bins`)
> are included.

## Commit Convention — **Semantic Commits Required**

This project uses **Conventional Commits** (https://www.conventionalcommits.org/)
for automated releases via `semantic-release`. Every commit message **must** follow
the format:

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

### Types

| Type | Meaning | Version bump |
|------|---------|--------------|
| `feat:` | New feature | Minor |
| `fix:` | Bug fix | Patch |
| `perf:` | Performance improvement | Patch |
| `refactor:` | Code change neither fixing nor adding features | Patch |
| `docs:` | Documentation only | Patch |
| `test:` | Test additions/changes | Patch |
| `chore:` | Maintenance (deps, CI, build, etc.) | Patch |
| `ci:` | CI/CD changes | Patch |
| `build:` | Build system / dependencies | Patch |

### Breaking Changes

Append `!` after the type/scope, **or** include `BREAKING CHANGE:` in the footer:

```
feat!: remove deprecated GHTS_OLD_VAR

BREAKING CHANGE: GHTS_OLD_VAR is no longer supported; use GHTS_NEW_VAR instead.
```

### Scopes (optional but recommended)

- `server` — server crate
- `client` — client crate
- `common` — common crate
- `testutil` — test utility crate
- `chart` — Helm chart
- `ci` — CI/CD pipeline
- `deploy` — Kubernetes manifests

### Examples

```
feat(server): add client IP and user agent logging to token endpoint
fix(client): honor Retry-After header on 429 responses
perf(github): reuse installation token for readiness probe
refactor(common): extract token masking to separate function
docs: update README with per-client key example
test(server): add integration test for rate limit rejection
chore(deps): update axum to 0.8
ci: add Grype vulnerability scan to release pipeline
```

### Why This Matters

- **semantic-release** parses commit messages on every push to `main`
- The version bump (major/minor/patch) is derived **automatically** from commit types
- CHANGELOG.md is generated automatically
- GitHub releases, container images, Helm chart, and binaries are published automatically
- Non-conforming commits break the release pipeline

## Branching Strategy & Release Flow

**Commits must be made on `develop` or feature branches, not directly on `main`.**

```
feature/xxx  ──PR──▶  develop  ──PR──▶  main (triggers release)
```

### README Update Gate — Required Before Every Push

Before **every `git push`**, review the README against the complete change being
pushed. Update the relevant README sections in the same branch whenever a
change affects documented behavior or user-facing interfaces, including
configuration, API/CLI usage, metrics and labels, Helm values, or deployment
instructions. Check examples and tables for accuracy as well. Do not push until
the README reflects those changes; avoid gratuitous edits when the change has
no README impact, but make that determination explicitly during the review.

### Workflow

1. **Create a branch** from `develop`:
   ```sh
   git checkout develop && git pull
   git checkout -b feat/my-feature
   ```

2. **Commit** using Conventional Commits format (see above)

3. **Push** your branch and open a **Pull Request** targeting `develop`

4. **CI runs** on the PR (tests, lint, build)

5. **Merge to `develop`** after review — this does **not** trigger a release

6. **When ready to release**, open a PR from `develop` → `main`

7. **Merge to `main`** — **this triggers `semantic-release`**:
   - Analyzes all commits since the last release tag
   - Determines version bump (major/minor/patch)
   - Creates Git tag `vX.Y.Z`
   - Updates `Cargo.toml` workspace version, `chart/Chart.yaml`, `package.json`
   - Publishes GitHub Release, container images, Helm chart, binaries

### Why This Matters

- **Releases only happen on `main`** — merging to `develop` is safe for integration
- **`develop` accumulates changes** until you're ready to cut a release
- **PRs enable review** and CI validation before code reaches `main`
- **semantic-release sees a clean history** of conventional commits on `main`

## Code Style

- **Rust 2024 edition**, workspace-wide `rustfmt.toml` (if present) or default style
- `clippy` with `-D warnings` — zero warnings in CI
- Prefer `anyhow::Result` for application errors, `thiserror` for library errors
- Structured logging with `tracing` — use fields, not string interpolation:
  ```rust
  info!(client = %name, ttl_secs = ttl, "token issued");  // ✓
  info!("token issued for {} with ttl {}", name, ttl);    // ✗
  ```
- Mask all GitHub tokens in logs (`mask_token`, `mask_gh_tokens` from `gh_token_common`)

## Testing

- Unit tests in `#[cfg(test)]` modules alongside code
- Integration tests in `server/tests/` and `client/tests/`
- Mock GitHub server (`testutil`) verifies RS256 JWT signatures and simulates:
  - `GET /app`, `GET /app/installations/{id}`, `POST /app/installations/{id}/access_tokens`
  - `GET /installation/repositories`, `GET /users/{login}`, `GET /rate_limit`
  - Per-endpoint failure injection and rate-limit responses

Run tests with `cargo test --workspace`. Integration tests require the mock server
to be reachable (it binds to a random port and sets `GHTS_GITHUB_BASE_URL`).

## Configuration

All server config via environment variables (see README). Key ones:

| Variable | Purpose |
|----------|---------|
| `GH_APP_ID`, `GH_APP_INSTALLATION_ID`, `GH_APP_KEY_B64` | Required GitHub App credentials |
| `GHTS_API_KEY` | Optional shared API key |
| `GHTS_CLIENT_KEYS` | JSON object of per-client keys |
| `GHTS_PURPOSE_KEY` | Make purpose part of cache key |
| `GHTS_RATE_MAX` / `GHTS_RATE_WINDOW` | Per-client rate limiting |
| `GHTS_BACKOFF_BASE_SECS` / `GHTS_BACKOFF_MAX_SECS` | GitHub rate-limit backoff |

## Release Process

1. Push conforming commits to `main`
2. GitHub Actions runs `cargo test --workspace`
3. `semantic-release` determines version bump from commits
4. Tags `vX.Y.Z`, updates `Cargo.toml` workspace version, `chart/Chart.yaml`, `package.json`
5. Publishes:
   - GitHub Release with binaries (linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64)
   - Container image `ghcr.io/runningman84/gh-token:vX.Y.Z` (multi-arch) + SBOM (Syft) + scan (Grype)
   - Helm chart to `oci://ghcr.io/runningman84/charts/gh-token`

## Common Pitfalls

- **Don't log raw tokens** — always use `mask_token()` / `mask_gh_tokens()`
- **Don't call `GET /user` with installation tokens** — they're `integration`-type, only `GET /installation/repositories` etc. work
- **Rate limit awareness** — the probe uses a cached installation token (not unauthenticated) to avoid the 60/h per-IP limit
- **Startup validation** — `/ready` returns 503 until credentials are validated; failures retry in background
- **Soft TTL vs GitHub TTL** — GitHub tokens always expire in 1 hour; server enforces shorter `soft_expires_at`

## Useful Commands

```sh
# Run server locally (needs GH_APP_* env vars)
cargo run -p gh-token-server

# Run client against local server
cargo run -p gh-token -- --client test-client

# Check formatting
cargo fmt --all -- --check

# Generate mock keypair for local testing
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out test.key
base64 -w0 test.key
```

## Pre-commit Hooks

A `.pre-commit-config.yaml` is provided. Install and enable:

```sh
pipx install pre-commit  # or: pip install pre-commit
pre-commit install       # enables git commit hooks
pre-commit install --hook-type commit-msg  # enables commit message validation
```

Run manually:

```sh
pre-commit run --all-files  # run all hooks on all files
pre-commit run cargo-clippy # run specific hook
```

Hooks included:
- **cargo fmt --check** — formatting
- **cargo clippy --workspace --all-targets -- -D warnings** — strict linting
- **cargo check --workspace** — compilation check
- **cargo test --workspace --lib --bins** — unit tests (fast)
- **ruff** — linting for TOML/JSON/YAML/MD
- **pre-commit-hooks** — trailing whitespace, EOF, YAML/JSON/TOML syntax, merge conflicts, line endings
- **commitizen** — validates Conventional Commits format on commit messages

> **Note**: The first run downloads Rust toolchains via rustup if needed. Subsequent runs are fast.
> Integration tests (`cargo test --workspace --tests`) are intentionally **not** in pre-commit for speed — they run in CI.