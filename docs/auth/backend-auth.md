# Authenticating with Object Store Backends

The proxy needs credentials to access backend object stores (S3, Azure Blob Storage, GCS). There are two approaches: static credentials stored in the proxy config, and OIDC-based credential resolution where the proxy acts as its own identity provider.

## Static Backend Credentials

The simplest approach is to include credentials directly in the bucket's `backend_options`:

```toml
[[buckets]]
name = "my-data"
backend_type = "s3"

[buckets.backend_options]
endpoint = "https://s3.us-east-1.amazonaws.com"
bucket_name = "my-backend-bucket"
region = "us-east-1"
access_key_id = "AKIAIOSFODNN7EXAMPLE"
secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
```

This works for any backend type. For anonymous backend access (e.g., public buckets), omit the credential fields and add `skip_signature = "true"`.

## OIDC Backend Auth

For production deployments, the proxy can act as its own OIDC identity provider. Instead of storing long-lived backend credentials, the proxy mints self-signed JWTs and exchanges them with cloud providers for temporary credentials — the same pattern used by GitHub Actions and Vercel for AWS access.

### How It Works

```mermaid
sequenceDiagram
    participant Client as S3 Client
    participant Proxy as Data Proxy
    participant Cloud as Cloud Provider STS<br/>(e.g., AWS STS)
    participant Store as Object Store<br/>(e.g., S3)

    Client->>Proxy: 1. S3 request (e.g., GET /bucket/key)
    Proxy->>Proxy: 2. Authenticate client
    Proxy->>Proxy: 3. Bucket has auth_type=oidc<br/>Mint self-signed JWT
    Proxy->>Cloud: 4. AssumeRoleWithWebIdentity<br/>(JWT + Role ARN)
    Cloud->>Proxy: 5. Fetch /.well-known/jwks.json
    Proxy-->>Cloud: 6. RSA public key
    Cloud-->>Proxy: 7. Temporary credentials
    Proxy->>Proxy: 8. Cache credentials
    Proxy->>Store: 9. Forward request with<br/>temporary credentials
    Store-->>Proxy: 10. Response
    Proxy-->>Client: 11. Response
```

### Configuration

OIDC backend auth requires two environment variables:

| Variable | Description |
|----------|-------------|
| `OIDC_PROVIDER_KEY` | PEM-encoded RSA private key for JWT signing |
| `OIDC_PROVIDER_ISSUER` | Publicly reachable URL (e.g., `https://data.source.coop`) |

Generate an RSA key pair:

```bash
openssl genrsa -out oidc-key.pem 2048
```

Set the environment variables:

```bash
export OIDC_PROVIDER_KEY=$(cat oidc-key.pem)
export OIDC_PROVIDER_ISSUER="https://data.source.coop"
```

Then configure buckets to use OIDC:

```toml
[[buckets]]
name = "my-data"
backend_type = "s3"

[buckets.backend_options]
endpoint = "https://s3.us-east-1.amazonaws.com"
bucket_name = "my-backend-bucket"
region = "us-east-1"
auth_type = "oidc"
oidc_role_arn = "arn:aws:iam::123456789012:role/DataProxyAccess"
```

### Discovery Endpoints

When OIDC provider keys are configured, the proxy serves two well-known endpoints that cloud providers use to validate JWTs:

**`GET /.well-known/openid-configuration`**
```json
{
  "issuer": "https://data.source.coop",
  "jwks_uri": "https://data.source.coop/.well-known/jwks.json",
  "response_types_supported": ["id_token"],
  "subject_types_supported": ["public"],
  "id_token_signing_alg_values_supported": ["RS256"]
}
```

**`GET /.well-known/jwks.json`**
```json
{
  "keys": [{
    "kty": "RSA",
    "alg": "RS256",
    "use": "sig",
    "kid": "proxy-key-1",
    "n": "<base64url-modulus>",
    "e": "<base64url-exponent>"
  }]
}
```

::: warning
These endpoints must be publicly accessible. Cloud providers fetch them at JWT validation time to verify signatures. If they are behind a firewall or VPN, credential exchange will fail.
:::

### The Exchange Flow in Detail

When a request arrives for a bucket with `auth_type=oidc`:

1. The `OidcBackendAuth` handler detects `auth_type=oidc` in the bucket's `backend_options`
2. It mints a short-lived JWT signed with the proxy's RSA private key:
   - `iss`: the configured `OIDC_PROVIDER_ISSUER`
   - `sub`: a connection identifier (from `oidc_subject` option, or a default)
   - `aud`: the cloud provider's STS audience (e.g., `sts.amazonaws.com`)
   - `exp`: short expiration (minutes)
3. The proxy sends the JWT to the cloud provider's STS endpoint along with the target IAM role ARN
4. The cloud provider fetches the proxy's JWKS, verifies the JWT signature, evaluates the role's trust policy, and returns temporary credentials
5. The proxy caches the credentials (keyed by role ARN) and injects them into the bucket config
6. The existing `build_object_store()` / `build_signer()` pipeline consumes the credentials normally

On subsequent requests, cached credentials are reused until they expire.

## Cloud Provider Setup

### AWS S3

**Administrator setup:**

1. **Register the OIDC provider** in your AWS account:
   ```bash
   aws iam create-open-id-connect-provider \
     --url https://data.source.coop \
     --client-id-list sts.amazonaws.com \
     --thumbprint-list <thumbprint>
   ```

   ::: tip
   To get the thumbprint, fetch the TLS certificate chain from your proxy's domain. AWS uses this to verify the HTTPS connection to the JWKS endpoint.
   :::

2. **Create an IAM Role** with a trust policy that allows the proxy to assume it:
   ```json
   {
     "Version": "2012-10-17",
     "Statement": [{
       "Effect": "Allow",
       "Principal": {
         "Federated": "arn:aws:iam::123456789012:oidc-provider/data.source.coop"
       },
       "Action": "sts:AssumeRoleWithWebIdentity",
       "Condition": {
         "StringEquals": {
           "data.source.coop:aud": "sts.amazonaws.com",
           "data.source.coop:sub": "s3-proxy"
         }
       }
     }]
   }
   ```

3. **Attach an S3 permission policy** to the role:
   ```json
   {
     "Version": "2012-10-17",
     "Statement": [{
       "Effect": "Allow",
       "Action": [
         "s3:GetObject",
         "s3:PutObject",
         "s3:ListBucket",
         "s3:DeleteObject"
       ],
       "Resource": [
         "arn:aws:s3:::my-backend-bucket",
         "arn:aws:s3:::my-backend-bucket/*"
       ]
     }]
   }
   ```

4. **Configure the bucket** in the proxy:
   ```toml
   [[buckets]]
   name = "my-data"
   backend_type = "s3"

   [buckets.backend_options]
   endpoint = "https://s3.us-east-1.amazonaws.com"
   bucket_name = "my-backend-bucket"
   region = "us-east-1"
   auth_type = "oidc"
   oidc_role_arn = "arn:aws:iam::123456789012:role/DataProxyAccess"
   ```

**At request time**, the proxy calls AWS STS `AssumeRoleWithWebIdentity` with the self-signed JWT. No AWS credentials are stored in the proxy configuration.

### Azure Blob Storage

::: info Planned
Azure OIDC backend auth is planned but not yet implemented. The proxy currently supports Azure with static credentials only.
:::

**Planned setup:**

1. Create an App Registration in Microsoft Entra ID
2. Add a Federated Identity Credential specifying the proxy's issuer URL and expected `sub` claim
3. Grant the app `Storage Blob Data Contributor` on the target storage account
4. The proxy would exchange its JWT for an Azure AD token via `client_credentials` grant with `jwt-bearer` assertion

### Google Cloud Storage

::: info Planned
GCS OIDC backend auth is planned but not yet implemented. The proxy currently supports GCS with static credentials only.
:::

**Planned setup:**

1. Create a Workload Identity Pool and OIDC Provider, specifying the proxy's issuer URL
2. Map the external identity to a GCP Service Account
3. Grant the service account GCS permissions
4. The proxy would use a two-step exchange: GCP STS token exchange, then `generateAccessToken` to impersonate the service account

## Credential Caching

When using OIDC backend auth, the proxy caches temporary credentials to avoid calling the cloud provider's STS on every request. Credentials are:

- Keyed by the IAM role ARN
- Automatically refreshed when they expire
- Shared across concurrent requests to the same bucket

This means the first request to an OIDC-backed bucket incurs a small latency cost for the credential exchange, but subsequent requests use cached credentials until they expire.

## Choosing Between Static and OIDC

| | Static Credentials | OIDC Backend Auth |
|---|---|---|
| **Setup complexity** | Low | Medium (IAM role + OIDC provider registration) |
| **Credential rotation** | Manual | Automatic (temporary credentials) |
| **Security** | Long-lived secrets in config | No long-lived secrets |
| **Cloud providers** | All (S3, Azure, GCS) | AWS S3 (Azure and GCS planned) |
| **Latency** | None | Small cost on first request (then cached) |
