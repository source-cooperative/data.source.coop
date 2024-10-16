# Source Cooperative Data Proxy

This repository contains the rust application which hosts the Source Cooperative Data Proxy.

## Getting Started

### Prerequisites
 - Cargo installed on your local machine
 - The AWS CLI installed on your local machine
 - An AWS Credentials profile with the name `opendata` which has permissions to push to the ECR repository and deploy to ECS

### Run Locally

To run the data proxy locally, run the following command:

```
./scripts/run.sh
```

## Deployment

Before you begin the deployment process, ensure that you have the `SOURCE_KEY` environment variable set with the production key.

### Tagging Release

After committing your changes, tag the release and bump the version with the following command:

```
./scripts/tag-release.sh
```

### Building and Pushing Image

To build and push the docker image to ECR, run the following command:

```
./scripts/build-push.sh
```

### Deploying to ECS

To deploy the image to ECS, run the following command:

```
./scripts/deploy.sh
```

### Rolling Back a Deployment

To roll back a deployment, first checkout the code for the version that you want to roll back to. For example:

```
git checkout v0.1.12
```

Next, deploy the version to ECS:

```
./scripts/deploy.sh
```
