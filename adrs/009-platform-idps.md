# ADR-009: Multi-Issuer Platform Identity Providers

**Status:** Proposed — not implemented
**Date:** 2026-08-09
**RFC:** RFC-001 §7
**Depends on:** ADR-004
**Blocks:** ADR-010

---

## Context

ADR-004 ships a `/.sts` exchange that trusts exactly one OIDC issuer: Source Cooperative's own Ory-based auth system, named by the `AUTH_ISSUER` environment variable.

This means the RFC's headline use case does not work. A GitHub Actions workflow holds an ambient OIDC token and needs no stored secret — that is the entire argument for the credential model in ADR-001 — but it cannot exchange that token, because its issuer is not `auth.source.coop`. The same applies to GitLab CI, Azure DevOps, HCP Terraform, and Vercel.

The blocker is small and specific: **`AUTH_ISSUER` is a single `String`, while `AUTH_AUDIENCE` is already parsed as a list.** The trust model beneath it — `RoleConfig.trusted_oidc_issuers` — is already a `Vec`. The exchange flow, JWKS cache, and credential minting need no change.

---

## Decision

### Platform IdPs

Source Cooperative operators pre-configure a set of well-known OIDC issuers relevant to data engineering workflows. These are immutable by users.

| Platform | Issuer URL | Key claims for constraints |
|---|---|---|
| Source Cooperative Auth | `auth.source.coop` | `sub`, `groups` |
| GitHub Actions | `https://token.actions.githubusercontent.com` | `repository`, `repository_owner`, `ref`, `environment`, `job_workflow_ref` |
| GitLab CI/CD | `https://gitlab.com` | `project_path`, `ref_type`, `environment` |
| Azure DevOps | `https://vstoken.dev.azure.com/<org_id>` | project, pipeline, environment |
| HCP Terraform | `https://app.terraform.io` | `terraform_workspace_id`, `terraform_run_phase` |
| Vercel | `https://oidc.vercel.com/<team_slug>` | `owner`, `project`, `environment` |

This list is illustrative, not exhaustive. Operators can add issuers without code changes.

Each platform IdP record:

```json
{
  "id": "github-actions",
  "issuer_url": "https://token.actions.githubusercontent.com",
  "display_name": "GitHub Actions",
  "well_known_claims": ["repository", "repository_owner", "ref", "environment", "job_workflow_ref"],
  "audience_hint": "https://data.source.coop"
}
```

`well_known_claims` provides documentation and UI hints — when a user creates a Role binding for this IdP (ADR-010), the UI can suggest these claims. `audience_hint` is the recommended `aud` value callers should request.

### Per-Issuer Audience Requirements

`AUTH_AUDIENCE` is currently a flat allowlist applied to the one trusted issuer. With several issuers, the audience requirement must become **per-issuer**: GitHub Actions tokens should carry `https://data.source.coop`, while Ory tokens carry an OAuth client ID. A flat global list would accept a GitHub token bearing the frontend's client ID, which is not a combination that should validate.

This is the one substantive design decision in this ADR; the rest is plumbing.

### Fail-Closed Behaviour Is Preserved

ADR-004's rule — an issuer with no audience restriction disables exchange rather than serving it unrestricted — must hold per issuer. An operator adding an issuer without an audience requirement should find that issuer refused, not silently trusted.

### Migration

1. Parse `AUTH_ISSUER` as a comma-separated list, mirroring `AUTH_AUDIENCE`; a single value remains valid, so existing deployments are unaffected.
2. Move the issuer→audience mapping into a structured variable, since a flat pair of lists cannot express per-issuer requirements.
3. Populate `trusted_oidc_issuers` on the `_default` Role from the parsed list.

Step 1 alone makes CI/CD workflows functional against the `_default` Role. Steps 2 and 3 are required before it is safe to enable in production.

> [!IMPORTANT]
> Until ADR-010 lands, every issuer added here can assume `_default`, whose ceiling is unlimited. A GitHub Actions workflow would receive the full permissions of whichever account the token's subject maps to. **Multi-issuer support without Roles widens the blast radius of any CI token to the user's entire account.** These two ADRs should ship together, or the issuer list should stay restricted to `auth.source.coop` until ADR-010 is ready.

### Downstream Clients Enabled

**GitHub Action** — `source-cooperative/configure-credentials` requests a GitHub OIDC token with audience `https://data.source.coop`, calls `/.sts`, and exports `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`:

```yaml
permissions:
  id-token: write
steps:
  - uses: source-cooperative/configure-credentials@v1
    with:
      role-urn: sc::my-org::role/github-publisher
  - run: aws s3 cp data.parquet s3://data.source.coop/my-org/my-product/
```

**CLI profiles** — role-specific `credential_process` entries in `~/.aws/config`:

```ini
[profile source-read]
credential_process = source-coop creds --role-arn sc::my-org::role/reader
```

**Anonymous web traffic** — the RFC proposed the Next.js server exchange its Vercel OIDC token for a platform Role scoped to public read-only access, so anonymous traffic flows through the full middleware stack. This is optional: anonymous reads work today without any exchange (ADR-005). Adopt it only if anonymous traffic needs attribution or rate limiting it cannot get otherwise.

> [!NOTE]
> **Future extension: account-registered IdPs.** Letting account owners register corporate identity systems (Okta, Entra ID), self-hosted providers (Keycloak), or any OIDC-compliant issuer without operator intervention. Requires a registration API with SSRF-safe URL validation, an account IdP storage table, and deletion guards when Roles reference the IdP. Deferred: it introduces SSRF risk on JWKS fetches to user-controlled URLs, DNS rebinding concerns, and self-asserted identity trust issues. The platform IdP list covers the primary use cases.

---

## Consequences

**Benefits**

- CI/CD and managed-compute workflows can authenticate with no stored secrets — the core promise of ADR-001.
- New issuers are a configuration change, not a code change.
- The exchange endpoint contract is unchanged; existing clients are unaffected.

**Costs / Risks**

- **Dangerous without ADR-010** (see the note above).
- Each trusted issuer is a trust relationship: a compromised issuer, or one that lets a caller choose their own `sub`, can mint identities the proxy will accept.
- Per-issuer audience configuration is more complex than a flat list and easy to get wrong; misconfiguration fails open toward accepting a token intended for another service.
- Adding an issuer needs operator access — a governance decision, not self-service.

---

## Alternatives Considered

**Keep a single issuer and require all automation to use Ory service accounts** — rejected. Reintroduces stored secrets for CI, which is exactly what ADR-001 set out to eliminate.

**Trust any issuer that presents a valid OIDC discovery document** — rejected. Self-asserted identity: anyone could stand up an issuer and mint any subject.

**A flat global audience allowlist across all issuers** — rejected. Would accept an (issuer, audience) pair that no legitimate client produces.
