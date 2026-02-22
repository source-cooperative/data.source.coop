# Source Data Proxy Architecture

## Data Proxy

The core function of this system is to operate an S3-compliant API that proxies requests to appropriate object storage backends (e.g. MinIO, AWS S3, Cloudflare R2, Azure Blobstore).

## Runtime

The system is designed to operate in various runtime environments. Chiefly, these includes operating as a traditional server running on a Linux server or containerized environment (e.g. ECS, K8s), or running in WASM on Cloudflare Workers.

## Authentication

### How clients authenticate with Source Data Proxy

The Source Data Proxy supports two forms of authentication:

1. Custom STS + registered Identity Providers
2. Long-term Access Keys

#### Custom STS + registered Identity Providers

The Source Data Proxy hosts a replica of the AWS Security Token Service. This service is used to exchange auth tokens (JWTs) from trusted OIDC-compatible identity providers (e.g. Source Cooperative's auth, Github workflows) for temporary scoped credentials. Those credentials can be used to make authenticated access to the Source Data Proxy.

For local development and CLI usage, users can obtain temporary credentials via a `credential_process` workflow:

1. User runs an AWS CLI command (e.g. `aws s3 ls s3://bucket/ --profile source-coop`)
2. The AWS SDK invokes a configured `credential_process` CLI tool
3. The CLI tool authenticates the user with the Source Cooperative's auth provider (e.g. browser-based login)
4. Upon successful login, the CLI tool receives an OIDC JWT from the auth provider
5. The CLI tool calls the Data Proxy's STS endpoint (`AssumeRoleWithWebIdentity`) with the JWT
6. The Data Proxy validates the JWT and returns temporary scoped credentials
7. The CLI tool outputs the credentials to stdout; the AWS SDK uses them transparently

The user's `~/.aws/config` would look like:

```ini
[profile source-coop]
credential_process = source credentials  # <- source cooperative cli
endpoint_url = https://data.source.coop
```

This approach reuses the existing `AssumeRoleWithWebIdentity` STS implementation and avoids the need to implement the full AWS SSO OIDC + Portal API surface (which `aws sso login` requires).

#### Long-term Access Credentials

For users that don't have access to OIDC identity providers, the Source Data Proxy can make use of long-term access keys (`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`). User can generate and retrieve these keys from the Source application (`https://source.coop`).

### How Source Data Proxy authenticates with object storage backends

To connect with backing object storage services (e.g. MinIO, AWS S3, Cloudflare R2, Azure Blobstore)

1. Custom OIDC Provider
2. Long-term Access Keys

#### Custom OIDC Provider

The Source Data Proxy operates as a custom OIDC Provider. Users can register this provider with their cloud environments. When the Source Data Proxy needs to connect with an object storage backend, it will generate a JWT signed with the Data Proxy's OIDC provider and use it to retrieve a set of temporary scoped credentials. To reduce latency, these credentials will be cached by the Source Data Proxy for reuse on subsequent requests. This process is akin to how Github or Vercel authenticates with AWS[^vercel-oidc][^github-oidc].

The proxy's OIDC discovery endpoints (`/.well-known/openid-configuration` and JWKS) must be publicly accessible, as cloud providers fetch them at token validation time to verify JWT signatures.

<details>

<summary>Cloud Provider Integration Workflows</summary>

##### AWS (S3)

**Administrator setup:**

1. Register the proxy's issuer URL (e.g. `https://data.source.coop`) as an IAM OIDC Identity Provider in the AWS account.
2. Create an IAM Role with a trust policy allowing `sts:AssumeRoleWithWebIdentity` from the provider, scoped by `aud` and `sub` claim conditions.
3. Attach a permission policy granting the necessary S3 access.

**At request time:**

1. The proxy mints a JWT with `iss: https://data.source.coop`, `sub: <connection-identifier>`, and `aud: sts.amazonaws.com`.
2. The proxy calls `AssumeRoleWithWebIdentity` on AWS STS with the JWT and the target Role ARN. This call does not require AWS credentials — the JWT is the sole authentication.
3. AWS validates the JWT (fetches JWKS, checks signature, evaluates trust policy conditions) and returns temporary `AccessKeyId` / `SecretAccessKey` / `SessionToken` credentials.
4. The proxy caches and passes these credentials to `AmazonS3Builder`.

##### Azure (Blob Storage)

**Administrator setup:**

1. Create an App Registration (or User-Assigned Managed Identity) in Microsoft Entra ID.
2. Add a Federated Identity Credential specifying the proxy's issuer URL and the expected `sub` claim value.
3. Grant the app registration a role assignment on the target storage account (e.g. `Storage Blob Data Contributor`).

**At request time:**

1. The proxy mints a JWT with `iss: https://data.source.coop`, `sub: <connection-identifier>`, and `aud: api://AzureADTokenExchange`.
2. The proxy exchanges the JWT for an Azure AD access token via the Microsoft identity platform token endpoint using `grant_type=client_credentials` with `client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer`. The JWT replaces a client secret.
3. Azure validates the JWT against the federated identity credential configuration and returns an OAuth 2.0 bearer token scoped to Azure Storage.
4. The proxy caches and passes the bearer token to `MicrosoftAzureBuilder`.

##### GCP (Cloud Storage)

**Administrator setup:**

1. Create a Workload Identity Pool and an OIDC Provider within it, specifying the proxy's issuer URL and an attribute mapping (e.g. `google.subject = assertion.sub`).
2. Grant the mapped external identity `roles/iam.workloadIdentityUser` on a GCP Service Account.
3. Grant the service account the necessary GCS permissions.

**At request time (two-step exchange):**

1. The proxy mints a JWT with `iss: https://data.source.coop`, `sub: <connection-identifier>`, and `aud` set to the Workload Identity Provider's full resource name.
2. The proxy calls the GCP STS endpoint (`sts.googleapis.com/v1/token`) with an RFC 8693 token exchange request, submitting the JWT as the subject token. GCP returns a federated access token.
3. The proxy uses the federated token to call the IAM Credentials API (`generateAccessToken`) to impersonate the service account, obtaining a short-lived OAuth 2.0 access token.
4. The proxy caches and passes the access token to `GoogleCloudStorageBuilder` via a custom `CredentialProvider`.

</details>

#### Long-term Access Credentials

For object storage backends that are unable to utilize the Source Data Proxy as an Identity Provider, the Data Proxy also stores long-term access credentials provided by the administrators of the object storage backend. These credentials will be used to authenticate when the Data Proxy needs to interact with the object storage backend.

[^vercel-oidc]: https://vercel.com/docs/oidc/aws
[^github-oidc]: https://docs.github.com/en/actions/concepts/security/openid-connect

## Modularity

The primary focus of this codebase is to serve as a data proxy for the [Source Cooperative](https://source.coop). However, it is built in a modular fashion to support reuse by others who have similar needs.