# Sealed Session Tokens

When the proxy mints temporary credentials via STS, it needs a way to recognize those credentials on subsequent requests. Sealed session tokens solve this by encrypting the full credential set into the session token itself — no server-side storage required.

## Why Sealed Tokens?

Traditional credential stores keep a mapping from access key ID to credentials on the server. This requires either a database or in-memory state, which is impractical for stateless runtimes like Cloudflare Workers.

Sealed tokens take a different approach: the credentials are encrypted and placed directly inside the session token that the client sends with every request. The proxy decrypts the token on each request to recover the credentials.

## How It Works

### Minting (seal)

When `AssumeRoleWithWebIdentity` mints temporary credentials:

1. The full `TemporaryCredentials` struct is serialized to JSON
2. A random 12-byte nonce is generated
3. The JSON is encrypted using AES-256-GCM with the nonce
4. The result is encoded as `base64url(nonce[12] || ciphertext + tag)`
5. This encoded string becomes the `SessionToken` returned to the client

### Verifying (unseal)

When a request arrives with an `x-amz-security-token` header:

1. The proxy base64url-decodes the session token
2. It extracts the nonce (first 12 bytes) and ciphertext (remainder)
3. It decrypts using AES-256-GCM with the configured key
4. The JSON is deserialized back to `TemporaryCredentials`
5. The proxy checks that the credentials haven't expired
6. The proxy verifies the request's SigV4 signature against the decrypted secret key

If the token doesn't look like a sealed token (e.g., not valid base64url), the proxy falls back to looking up credentials from the config provider.

## Configuration

Set the `SESSION_TOKEN_KEY` environment variable to a base64-encoded 32-byte key:

```bash
# Generate a key
openssl rand -base64 32

# Set it
export SESSION_TOKEN_KEY="<base64-encoded-32-byte-key>"
```

This key must be the same across all instances of the proxy. If you rotate the key, all existing session tokens become invalid — clients will need to re-authenticate.

::: warning
`SESSION_TOKEN_KEY` is required for the Cloudflare Workers runtime. Without it, temporary credentials from STS cannot be verified on subsequent requests.
:::

## Scope Behavior

Access scopes are sealed into the token at mint time. This means:

- Changing a role's `allowed_scopes` in the config only affects newly minted credentials
- Existing session tokens continue to use the scopes they were minted with until they expire
- There is no way to revoke a sealed token short of rotating the encryption key (which invalidates all tokens)

## Security Properties

- **Confidentiality**: AES-256-GCM encryption prevents clients from reading or modifying the sealed credentials
- **Integrity**: The GCM authentication tag detects any tampering with the ciphertext
- **Replay protection**: Each token has a random nonce; however, tokens are valid until their expiration time
- **Constant-time comparison**: The access key ID verification uses constant-time comparison to prevent timing attacks
