# source-coop CLI

Authenticate with the Source Cooperative data proxy and obtain temporary S3 credentials.

Uses the OAuth2 Authorization Code flow with PKCE to authenticate via browser, then exchanges the OIDC ID token at the proxy's STS endpoint for temporary AWS credentials.

## Install

```bash
cargo install --path crates/cli
```

## Usage

```bash
source-coop login --role-arn <ARN>
```

This opens your browser to the Source Cooperative login page. After authenticating, temporary S3 credentials are printed to stdout.

### Options

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--issuer` | `SOURCE_OIDC_ISSUER` | `https://auth.source.coop` | OIDC issuer URL |
| `--client-id` | `SOURCE_OIDC_CLIENT_ID` | `d037d00b-...` | OAuth2 client ID |
| `--proxy-url` | `SOURCE_PROXY_URL` | `http://localhost:8787` | S3 proxy URL for STS |
| `--role-arn` | `SOURCE_ROLE_ARN` | *(required)* | Role ARN to assume |
| `--format` | | `credential-process` | Output format: `credential-process` or `env` |
| `--duration` | | | Session duration in seconds |
| `--scope` | | `openid` | OAuth2 scopes |
| `--port` | | `0` (random) | Local callback port |

### Output formats

**credential-process** (default) — for use with `~/.aws/config`:

```ini
[profile source-coop]
credential_process = source-coop login --role-arn <ARN>
```

**env** — for shell eval:

```bash
eval $(source-coop login --role-arn <ARN> --format env)
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
