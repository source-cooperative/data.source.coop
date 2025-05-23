# Source Cooperative Data Proxy

This repository contains the rust application which hosts the Source Cooperative Data Proxy.

## Getting Started

### Prerequisites

- Cargo installed on your local machine
- The AWS CLI installed on your local machine

### Run Locally

To run the data proxy locally, run the following command:

```
./scripts/run.sh
```

### Contributing

To make contributions to this codebase, please create a pull request of a feature branch to the `main` branch. The PR title should conform to [Conventional Commits](http://conventionalcommits.org/en/v1.0.0/).

> [!TIP]
> The `CHANGELOG.md` and the project version within `Cargo.toml` are managed automatically within our CICD pipeline. There is typically no need for individual developers to alter these values.

### Releases

Releases are automated via the [Release Please action](https://github.com/googleapis/release-please-action/). As contributions are made to `main`, a release PR will be kept up-to-date to represent the upcoming release. When that PR is merged, a new Github Release will be generated.

### Deployments

Merges to the `main` branch trigger deployment to the development instance of the proxy.

New releases trigger deployment to the production instance of the proxy.

<details>

<summary>Manual Deployment Steps</summary>

**⚠️ Manual deployment should only be necessary in extreme circumstances. Automated deployments via GitHub Workflows are preferred. ⚠️**

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

</details>
