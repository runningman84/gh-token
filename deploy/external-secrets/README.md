# Individual agent credentials with External Secrets Operator

This example generates a credential **once** in namespace `agent-a`, then copies
that exact value into namespace `gh-token`. Repeat with independent names and
credentials for each agent. The chart does not install ESO or manage cross-namespace
RBAC; keep these resources in your platform/GitOps configuration.

Prerequisites: both namespaces exist, ESO is installed with the Password generator
and Kubernetes provider, and the controller is allowed to reconcile both namespaces
and request tokens for the SecretStore service account. These examples use ESO
`external-secrets.io/v1`; check the CRD versions installed in your cluster.

## 1. Generate the agent's canonical credential

Apply this as plain YAML (if embedding in Helm, escape ESO's template expression).
`CreatedOnce` avoids generating a new password every reconciliation. Do not create
another generator-backed ExternalSecret for the gh-token copy: it would generate
an independent password, not the same value.

```yaml
apiVersion: generators.external-secrets.io/v1alpha1
kind: Password
metadata:
  name: gh-token-key
  namespace: agent-a
spec:
  length: 48
  digits: 12
  symbols: 0
  allowRepeat: true
---
apiVersion: external-secrets.io/v1
kind: ExternalSecret
metadata:
  name: gh-token-auth
  namespace: agent-a
spec:
  refreshPolicy: CreatedOnce
  target:
    name: gh-token-auth
    creationPolicy: Owner
    template:
      engineVersion: v2
      data:
        api-key: "{{ .password }}"
  dataFrom:
    - sourceRef:
        generatorRef:
          apiVersion: generators.external-secrets.io/v1alpha1
          kind: Password
          name: gh-token-key
```

## 2. Copy only this credential to the server namespace

The SecretStore uses a dedicated service account in `gh-token`, not the gh-token
server's service account. Grant it read access to the one canonical Secret in
`agent-a`; neither the server nor agent workload needs Kubernetes API access.
Permission-review access is cluster-scoped and does not itself grant Secret access.

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: agent-a-secret-reader
  namespace: gh-token
automountServiceAccountToken: false
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: gh-token-key-reader
  namespace: agent-a
rules:
  - apiGroups: [""]
    resources: ["secrets"]
    resourceNames: ["gh-token-auth"]
    verbs: ["get"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: gh-token-key-reader
  namespace: agent-a
subjects:
  - kind: ServiceAccount
    name: agent-a-secret-reader
    namespace: gh-token
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: gh-token-key-reader
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: gh-token-eso-permission-review
rules:
  - apiGroups: ["authorization.k8s.io"]
    resources: ["selfsubjectrulesreviews", "selfsubjectaccessreviews"]
    verbs: ["create"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: gh-token-eso-agent-a-review
subjects:
  - kind: ServiceAccount
    name: agent-a-secret-reader
    namespace: gh-token
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: gh-token-eso-permission-review
---
apiVersion: external-secrets.io/v1
kind: SecretStore
metadata:
  name: agent-a-credentials
  namespace: gh-token
spec:
  provider:
    kubernetes:
      remoteNamespace: agent-a
      server:
        caProvider:
          type: ConfigMap
          name: kube-root-ca.crt
          key: ca.crt
      auth:
        serviceAccount:
          name: agent-a-secret-reader
---
apiVersion: external-secrets.io/v1
kind: ExternalSecret
metadata:
  name: agent-a-auth
  namespace: gh-token
spec:
  refreshInterval: 1m
  secretStoreRef:
    kind: SecretStore
    name: agent-a-credentials
  target:
    name: agent-a-auth
    creationPolicy: Owner
  data:
    - secretKey: api-key
      remoteRef:
        key: gh-token-auth
        property: api-key
```

Use explicit `data` lookups, not `dataFrom.find` (which would require listing other
Secrets). Restrict who can create workloads, service-account tokens, SecretStores,
and ExternalSecrets in these namespaces. Workloads able to impersonate the reader
service account can read the credential. Repeat the per-agent Role, RoleBinding,
service account, store and ExternalSecret for other agents; the permission-review
ClusterRole can be reused.

## 3. Configure the chart and agent

Server Helm values (Secret references only, never literal passwords):

```yaml
clientAuth:
  enabled: true
  requireKnownClient: true
  clients:
    - name: agent-a
      secretRef:
        name: agent-a-auth
        key: api-key
```

Agent container environment (assuming service name `gh-token`):

```yaml
env:
  - name: GHTS_URL
    value: http://gh-token.gh-token.svc.cluster.local:8080
  - name: GHTS_CLIENT
    value: agent-a
  - name: GHTS_API_KEY
    valueFrom:
      secretKeyRef:
        name: gh-token-auth
        key: api-key
```

Wait for **both** ExternalSecrets to report Ready before deploying workloads. Use
a stable globally unique agent name (1–64 lowercase ASCII letters, digits, `_`,
`-`). Every agent must have its own generated key; duplicate keys are rejected by
the server. A key for `agent-a` cannot request tokens for `agent-b` or an unknown
name. Set neither `secret.apiKey` nor `env.GHTS_API_KEY` on the server in this mode.

Enable the chart's NetworkPolicy/Cilium policy for the agent namespaces and use TLS
or service-mesh encryption when required. NetworkPolicy alone does not encrypt keys.

## Rotation, revocation and investigations

- This initial implementation loads credentials at **server startup**; changes to
  mounted files are not hot-reloaded. Agent environment variables also require a
  restart. Coordinate rotation: update the canonical credential using your ESO
  lifecycle procedure, wait for the copy to synchronize, then roll out the server
  and affected agents. Allow temporary authentication failures; there is no
  dual-key grace period. For generated CreatedOnce credentials, deletion/recreation
  can generate a replacement; use this only as a planned rotation, not routine sync.
- Removing an agent requires removing its chart entry and rolling out the server.
  Removing its Secret alone is not immediate revocation of an already running
  server's in-memory key. Revoke compromised GitHub tokens separately; rejecting
  an API key does not invalidate previously issued GitHub tokens.
- gh-token creates/caches tokens separately by client ID. Logs associate the
  authenticated client with the SHA-256 `token_fingerprint` on issuance and cache
  delivery. Background renewal logs connect old and new fingerprints. Keep logs
  from all server replicas, with timestamps and pod metadata, in your audit store.
- A token fingerprint is SHA-256 of the exact token bytes, lowercase hex, without
  a newline. Compute it only in a secure investigation environment; do not put
  tokens into shell history, command arguments, tickets or logs.
- Different installation tokens still use the same GitHub App bot identity and
  permissions. This provides local token-to-agent evidence, **not** guaranteed
  per-agent attribution in every GitHub audit event. Verify your GitHub audit
  capabilities; use separate Apps/installations if distinct actors are necessary.

References: [ESO password generator](https://external-secrets.io/latest/api/generator/password/),
[ESO Kubernetes provider](https://external-secrets.io/latest/provider/kubernetes/).
