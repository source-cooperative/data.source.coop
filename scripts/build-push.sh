VERSION=$(cargo metadata --format-version=1 --no-deps | jq -r '.packages[0].version')
docker buildx build --platform linux/arm64 -t 417712557820.dkr.ecr.us-west-2.amazonaws.com/source-data-proxy:v$VERSION --push .
aws ecr get-login-password --region us-west-2 --profile opendata | docker login --username AWS --password-stdin 417712557820.dkr.ecr.us-west-2.amazonaws.com
docker push 417712557820.dkr.ecr.us-west-2.amazonaws.com/source-data-proxy:v$VERSION
