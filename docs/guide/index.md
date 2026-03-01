# User Guide

The Source Data Proxy provides S3-compatible access to data stored across multiple cloud backends. You interact with it using standard S3 tools — `aws-cli`, `boto3`, or any S3-compatible SDK — just point the endpoint URL at the proxy.

## Getting Started

1. **[Authentication](./authentication)** — How to authenticate and obtain credentials
2. **[Client Usage](./client-usage)** — Using aws-cli, boto3, curl, and other S3 clients

## Quick Example

```bash
# Anonymous access to a public bucket
curl https://data.source.coop/public-data/hello.txt

# Authenticated access with the CLI
source-coop login
aws s3 ls s3://my-bucket/ --profile source-coop
```

## How It Works

The proxy sits between your S3 client and the backend object stores. You send standard S3 requests to the proxy, and it handles authentication, authorization, and forwarding to the correct backend. From your perspective, it behaves like any other S3-compatible service.
