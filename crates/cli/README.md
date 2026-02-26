# source-coop CLI

Authenticate with the Source Cooperative data proxy and obtain temporary S3 credentials.

Uses the OAuth2 Authorization Code flow with PKCE to authenticate via browser, then exchanges the OIDC ID token at the proxy's STS endpoint for temporary AWS credentials.

## Install

```bash
cargo install --path crates/cli
```

## Usage

### Recommended: login + credential-process

1. Log in once (opens browser, caches credentials to `~/.source-coop/credentials/`):

```bash
source-coop login
```

2. Configure `~/.aws/config` to use cached credentials:

```ini
[profile source-coop]
credential_process = source-coop credential-process
endpoint_url = https://data.source.coop
```

3. Use AWS tools normally:

```bash
aws s3 ls s3://my-bucket/ --profile source-coop
```

When credentials expire, run `source-coop login` again.

### Multiple roles

Each role's credentials are cached separately:

```bash
source-coop login --role-arn reader-role
source-coop login --role-arn admin-role
```

```ini
[profile source-coop]
credential_process = source-coop credential-process --role-arn reader-role
endpoint_url = https://data.source.coop

[profile source-coop-admin]
credential_process = source-coop credential-process --role-arn admin-role
endpoint_url = https://data.source.coop
```

### Login options

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--issuer` | `SOURCE_OIDC_ISSUER` | `https://auth.source.coop` | OIDC issuer URL |
| `--client-id` | `SOURCE_OIDC_CLIENT_ID` | `d037d00b-...` | OAuth2 client ID |
| `--proxy-url` | `SOURCE_PROXY_URL` | `https://data.source.coop` | S3 proxy URL for STS |
| `--role-arn` | `SOURCE_ROLE_ARN` | `source-coop-user` | Role ARN to assume |
| `--format` | | `credential-process` | Output format: `credential-process` or `env` |
| `--duration` | | | Session duration in seconds |
| `--scope` | | `openid` | OAuth2 scopes |
| `--port` | | `0` (random) | Local callback port |

### Output formats

In addition to caching, `login` prints credentials to stdout:

**credential-process** (default) — AWS credential_process JSON:

```bash
source-coop login
```

**env** — shell export statements:

```bash
eval $(source-coop login --format env)
```

## OIDC provider setup

The CLI uses the OAuth2 Authorization Code flow with PKCE. It starts a temporary local server on `http://127.0.0.1:{port}/callback` to receive the authorization code redirect.

The OAuth2 client must have a matching redirect URI registered. There are two approaches:

### Option A: Allow any port (recommended)

Register `http://127.0.0.1/callback` as a redirect URI on the OAuth2 client. Per [RFC 8252 Section 7.3](https://datatracker.ietf.org/doc/html/rfc8252#section-7.3), loopback redirect URIs should allow any port. Ory Network follows this convention — registering the base URI without a port permits any port.

The CLI defaults to `--port 0` (OS-assigned random available port), which works with this setup.

### Option B: Fixed port

Register a specific redirect URI (e.g. `http://127.0.0.1:8400/callback`) and run the CLI with the matching port:

```bash
source-coop login --role-arn <ARN> --port 8400
```

### Client configuration

The OAuth2 client should be configured as a **public client** (no client secret) with:

- **Grant type**: Authorization Code
- **Token endpoint auth method**: `none` (public client, PKCE used instead)
- **Allowed scopes**: `openid`
- **Redirect URIs**: `http://127.0.0.1/callback` (see above)
