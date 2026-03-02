# Client Usage

The proxy exposes a standard S3-compatible API. Any S3 client works — just set the endpoint URL to point at the proxy.

## aws-cli

```bash
# Download a file
aws s3 cp s3://my-bucket/path/to/file.txt ./file.txt \
    --endpoint-url https://data.source.coop

# Upload a file
aws s3 cp ./local-file.txt s3://my-bucket/uploads/file.txt \
    --endpoint-url https://data.source.coop

# List bucket contents
aws s3 ls s3://my-bucket/prefix/ \
    --endpoint-url https://data.source.coop
```

### Using AWS Profiles

Add a profile to `~/.aws/config` to avoid specifying the endpoint every time:

```ini
[profile source-coop]
credential_process = source-coop creds
endpoint_url = https://data.source.coop
```

Then use it:

```bash
aws s3 ls s3://my-bucket/ --profile source-coop
aws s3 cp s3://my-bucket/data.csv ./data.csv --profile source-coop
```

See [Authentication](./authentication) for setting up credentials and profiles.

## boto3 (Python)

```python
import boto3

s3 = boto3.client(
    "s3",
    endpoint_url="https://data.source.coop",
    aws_access_key_id="AKPROXY00000EXAMPLE",
    aws_secret_access_key="proxy/secret/key/EXAMPLE",
)

# Download
s3.download_file("my-bucket", "path/to/file.txt", "./file.txt")

# Upload
s3.upload_file("./local-file.txt", "my-bucket", "uploads/file.txt")

# List
response = s3.list_objects_v2(Bucket="my-bucket", Prefix="prefix/")
for obj in response.get("Contents", []):
    print(obj["Key"])
```

### Using a Profile with boto3

If you have an AWS profile configured with `credential_process`:

```python
import boto3

session = boto3.Session(profile_name="source-coop")
s3 = session.client("s3")

response = s3.list_objects_v2(Bucket="my-bucket")
```

## curl

For anonymous buckets, you can use curl directly:

```bash
# Download
curl https://data.source.coop/public-data/hello.txt

# HEAD request (metadata only)
curl -I https://data.source.coop/public-data/hello.txt
```

> [!NOTE]
> Authenticated requests require SigV4 signing. Use aws-cli or an SDK rather than raw curl.

## Request Styles

The proxy supports two S3 URL styles:

### Path Style (default)

```
https://data.source.coop/bucket-name/key/path
```

This is the default and works without additional configuration.

### Virtual-Hosted Style

```
https://bucket-name.s3.example.com/key/path
```

Virtual-hosted style requires that the proxy administrator has configured the `--domain` flag. The proxy extracts the bucket name from the `Host` header.
