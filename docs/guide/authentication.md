# Authentication

The proxy supports three ways to authenticate, depending on your use case.

## Anonymous Access

Public buckets serve read requests without credentials:

```bash
curl https://data.source.coop/public-data/hello.txt
```

Anonymous access only allows `GetObject`, `HeadObject`, and `ListBucket` operations. Write operations always require authentication.

## Long-Lived Access Keys

If your administrator has issued you a static access key pair, use them like standard AWS credentials:

```bash
AWS_ACCESS_KEY_ID=AKPROXY00000EXAMPLE \
AWS_SECRET_ACCESS_KEY="proxy/secret/key/EXAMPLE000000000000" \
aws s3 cp s3://my-bucket/path/to/file.txt ./file.txt \
    --endpoint-url https://data.source.coop
```

These work with any S3-compatible client. The proxy verifies requests using standard AWS SigV4 signing, so no special client configuration is needed beyond setting the endpoint URL.

## OIDC / STS Temporary Credentials

This is the recommended authentication method. You exchange a JWT from your organization's identity provider for scoped, time-limited credentials — the same flow as AWS `AssumeRoleWithWebIdentity`.

There are two ways to do this: the CLI (for interactive use) and direct STS calls (for CI/CD and scripts).

### CLI Authentication

The `source-coop` CLI handles the OIDC flow for you. It opens your browser, authenticates with your identity provider, and obtains temporary credentials.

**Install the CLI:**

```bash
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/source-cooperative/source-coop-cli/releases/latest/download/source-coop-cli-installer.sh | sh

# Or from source
cargo install --git https://github.com/source-cooperative/source-coop-cli
```

**Log in:**

```bash
source-coop login
```

This opens your browser to authenticate. Once complete, credentials are cached in your OS keyring.

### AWS Profile Integration

Set up an AWS profile to use the proxy seamlessly with standard AWS tools:

```ini
[profile source-coop]
credential_process = source-coop creds
endpoint_url = https://data.source.coop
```

Then use AWS tools normally — credentials are obtained and refreshed automatically:

```bash
aws s3 ls s3://my-bucket/ --profile source-coop
aws s3 cp ./data.csv s3://my-bucket/uploads/data.csv --profile source-coop
```

### Multiple Roles

If your administrator has set up multiple roles with different access scopes, you can create a profile for each:

```bash
source-coop login --role-arn reader-role
source-coop login --role-arn admin-role
```

```ini
[profile sc-reader]
credential_process = source-coop creds --role-arn reader-role
endpoint_url = https://data.source.coop

[profile sc-admin]
credential_process = source-coop creds --role-arn admin-role
endpoint_url = https://data.source.coop
```

### CLI Options

| Flag          | Env Var                 | Default                    | Description                           |
| ------------- | ----------------------- | -------------------------- | ------------------------------------- |
| `--issuer`    | `SOURCE_OIDC_ISSUER`    | `https://auth.source.coop` | OIDC issuer URL                       |
| `--client-id` | `SOURCE_OIDC_CLIENT_ID` | (built-in)                 | OAuth2 client ID                      |
| `--proxy-url` | `SOURCE_PROXY_URL`      | `https://data.source.coop` | Proxy URL for STS                     |
| `--role-arn`  | `SOURCE_ROLE_ARN`       | `source-coop-user`         | Role ARN to assume                    |
| `--format`    |                         | `credential-process`       | Output: `credential-process` or `env` |
| `--duration`  |                         | (role default)             | Session duration in seconds           |
| `--scope`     |                         | `openid`                   | OAuth2 scopes                         |
| `--port`      |                         | `0` (random)               | Local callback server port            |
| `--no-cache`  |                         |                            | Skip caching credentials              |

### Direct STS Exchange

After logging in, you can export cached credentials as environment variables:

```bash
eval $(source-coop creds --format env)

# Credentials are now exported — use any S3 client
aws s3 cp ./data.csv s3://deploy-bundles/data.csv \
    --endpoint-url https://data.source.coop
```

You can also call the STS endpoint directly with a JWT:

```bash
CREDS=$(aws sts assume-role-with-web-identity \
    --role-arn github-actions-deployer \
    --web-identity-token "$JWT_TOKEN" \
    --endpoint-url https://data.source.coop \
    --output json)

export AWS_ACCESS_KEY_ID=$(echo $CREDS | jq -r '.Credentials.AccessKeyId')
export AWS_SECRET_ACCESS_KEY=$(echo $CREDS | jq -r '.Credentials.SecretAccessKey')
export AWS_SESSION_TOKEN=$(echo $CREDS | jq -r '.Credentials.SessionToken')
```

The STS endpoint accepts a JWT from any OIDC provider that your administrator has configured as trusted. See the [Administration guide](/auth/proxy-auth) for details on setting up identity providers and trust policies.
