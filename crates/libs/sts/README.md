# source-coop-sts

OIDC token exchange and STS credential minting for the S3 proxy gateway. Implements the `AssumeRoleWithWebIdentity` flow, allowing workloads like GitHub Actions to exchange OIDC JWTs for temporary, scoped S3 credentials.

## What This Crate Does

```
GitHub Actions (or any OIDC provider)
    │
    │  JWT (signed by provider)
    ▼
┌─────────────────────────────┐
│  source-coop-sts           │
│                             │
│  1. Decode JWT header       │
│  2. Fetch JWKS from issuer  │
│  3. Verify JWT signature    │
│  4. Check trust policy:     │
│     - issuer ∈ trusted?     │
│     - audience matches?     │
│     - subject matches glob? │
│  5. Mint temporary creds    │
│     (AccessKeyId,           │
│      SecretAccessKey,       │
│      SessionToken)          │
│  6. Store via ConfigProvider│
└─────────────────────────────┘
    │
    │  TemporaryCredentials
    ▼
Client signs S3 requests with temp creds
```

## Runtime Coupling

This crate uses `reqwest` for JWKS fetching, which works on both native and WASM targets (`reqwest` compiles to `wasm32-unknown-unknown` using `web-sys` fetch). It does not depend on Tokio directly; the async functions are runtime-agnostic.

If you need to use a different HTTP client for JWKS fetching (e.g., the Workers Fetch API directly), you'd replace the `fetch_jwks` function in `jwks.rs` or introduce a trait for HTTP fetching. This is a reasonable follow-up if WASM binary size becomes a concern.

## Module Overview

```
src/
├── lib.rs       Entry point: assume_role_with_web_identity(), subject glob matching
├── request.rs   STS request parsing (AssumeRoleWithWebIdentity query params)
├── responses.rs STS XML response serialization
├── jwks.rs      JWKS fetching, JWK parsing, JWT signature verification
└── sts.rs       Temporary credential minting (AccessKeyId/SecretAccessKey/SessionToken)
```

## Usage

Called by the proxy handler when it receives an STS `AssumeRoleWithWebIdentity` request:

```rust
use source_coop_sts::assume_role_with_web_identity;
use source_coop_sts::request::{StsRequest, try_parse_sts_request};

// Parse from query string
let sts_request = try_parse_sts_request(Some(query))
    .transpose()?       // Option<Result<..>> → Result<Option<..>>
    .expect("STS request");

let creds = assume_role_with_web_identity(
    &config_provider,
    &sts_request,
    "TEMPKEY",            // key prefix for minted credentials
).await?;

// creds.access_key_id, creds.secret_access_key, creds.session_token
// are returned to the client in an STS XML response.
```

## Trust Policies

Roles define trust policies in the config:

- **`trusted_oidc_issuers`** — which OIDC providers are accepted (e.g., `https://token.actions.githubusercontent.com`)
- **`required_audience`** — the `aud` claim the JWT must contain
- **`subject_conditions`** — glob patterns matched against the `sub` claim (e.g., `repo:myorg/myrepo:ref:refs/heads/main`, `repo:myorg/*`)
- **`allowed_scopes`** — buckets, prefixes, and actions the minted credentials grant access to
