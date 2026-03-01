# Supported Operations

## S3 Operations

| Operation | HTTP Method | Dispatch | Description |
|-----------|------------|----------|-------------|
| GetObject | `GET /{bucket}/{key}` | Forward | Download a file |
| HeadObject | `HEAD /{bucket}/{key}` | Forward | Get file metadata |
| PutObject | `PUT /{bucket}/{key}` | Forward | Upload a file |
| DeleteObject | `DELETE /{bucket}/{key}` | Forward | Delete a file |
| ListBucket | `GET /{bucket}` | Response | List objects in a bucket (ListObjectsV2) |
| ListBuckets | `GET /` | Response | List all virtual buckets |
| CreateMultipartUpload | `POST /{bucket}/{key}?uploads` | NeedsBody | Initiate a multipart upload |
| UploadPart | `PUT /{bucket}/{key}?partNumber=N&uploadId=ID` | NeedsBody | Upload a part |
| CompleteMultipartUpload | `POST /{bucket}/{key}?uploadId=ID` | NeedsBody | Complete a multipart upload |
| AbortMultipartUpload | `DELETE /{bucket}/{key}?uploadId=ID` | NeedsBody | Abort a multipart upload |

### Dispatch Types

- **Forward** — A presigned URL is generated and returned to the runtime, which executes it with its native HTTP client. Bodies stream directly between client and backend without buffering.
- **Response** — The handler builds a complete response (XML for LIST, error responses) and returns it. No presigned URL involved.
- **NeedsBody** — The runtime collects the request body, then the handler signs and sends the request via raw HTTP (`backend.send_raw()`). Multipart only.

## STS Operations

| Operation | HTTP Method | Description |
|-----------|------------|-------------|
| AssumeRoleWithWebIdentity | `POST /?Action=AssumeRoleWithWebIdentity&...` | Exchange OIDC JWT for temporary credentials |

## OIDC Discovery Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/.well-known/openid-configuration` | GET | OpenID Connect discovery document |
| `/.well-known/jwks.json` | GET | JSON Web Key Set (proxy's RSA public key) |

These are served when `OIDC_PROVIDER_KEY` and `OIDC_PROVIDER_ISSUER` are configured.

## Limitations

- **LIST returns all results** — `object_store::list_with_delimiter()` fetches all pages internally. `IsTruncated` is always `false`. Continuation tokens and max-keys are not supported.
- **Multipart is S3 only** — Multipart operations use raw HTTP with `S3RequestSigner` and are gated to `backend_type = "s3"`. Non-S3 backends should use single PUT requests.
- **DeleteObject does not return confirmation** — The proxy forwards the DELETE and returns the backend's response status.
