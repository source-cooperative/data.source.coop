# Error Codes

The proxy returns S3-compatible error responses in XML format:

```xml
<Error>
  <Code>AccessDenied</Code>
  <Message>Access Denied</Message>
  <RequestId>550e8400-e29b-41d4-a716-446655440000</RequestId>
</Error>
```

## Error Types

| Error | HTTP Status | S3 Code | When |
|-------|------------|---------|------|
| BucketNotFound | 404 | `NoSuchBucket` | Requested bucket doesn't exist in config |
| NoSuchKey | 404 | `NoSuchKey` | Key not found in backend (forwarded from backend response) |
| AccessDenied | 403 | `AccessDenied` | Caller lacks permission for the requested operation |
| SignatureDoesNotMatch | 403 | `SignatureDoesNotMatch` | SigV4 signature verification failed |
| MissingAuth | 403 | `AccessDenied` | Authentication required but no credentials provided |
| ExpiredCredentials | 403 | `ExpiredToken` | Temporary credentials have expired |
| InvalidOidcToken | 400 | `InvalidIdentityToken` | JWT validation failed (bad signature, untrusted issuer, etc.) |
| RoleNotFound | 403 | `AccessDenied` | Requested role doesn't exist in config |
| InvalidRequest | 400 | `InvalidRequest` | Malformed S3 request |
| BackendError | 503 | `ServiceUnavailable` | Backend object store is unreachable or returned an error |
| PreconditionFailed | 412 | `PreconditionFailed` | Conditional request failed (If-Match, etc.) |
| NotModified | 304 | `NotModified` | Conditional request — content not changed |
| ConfigError | 500 | `InternalError` | Invalid proxy configuration |
| Internal | 500 | `InternalError` | Unexpected internal error |

## STS Error Responses

STS errors follow the AWS STS error format:

```xml
<ErrorResponse>
  <Error>
    <Code>InvalidIdentityToken</Code>
    <Message>Token signature verification failed</Message>
  </Error>
  <RequestId>550e8400-e29b-41d4-a716-446655440000</RequestId>
</ErrorResponse>
```

| HTTP Status | Code | When |
|------------|------|------|
| 400 | `MalformedPolicyDocument` | Role not found in config |
| 400 | `InvalidIdentityToken` | JWT invalid, untrusted issuer, algorithm unsupported, subject mismatch |
| 400 | `InvalidParameterValue` | Missing required STS parameters |
| 403 | `AccessDenied` | General authorization failure |
| 500 | `InternalError` | Unexpected error during token exchange |

## Error Message Safety

For 5xx errors, the proxy returns generic messages to avoid leaking internal infrastructure details. The full error message is logged server-side but not exposed to clients.

For 4xx errors, the proxy returns descriptive messages to help clients debug authentication and authorization issues.
