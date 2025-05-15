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

Contributing new features should be and deploying new versions of the proxy should be done as follows:

1. Create a pull request of a feature branch to the `dev` branch. Either the commit message (in the event that a single commit is made) or the PR title (in the event that multiple commits were made) should conform to [Conventional Commits](http://conventionalcommits.org/en/v1.0.0/).

### Release + Deployment

Releasing and deploying new versions of the proxy should be done as follows:

1. Create a pull request of a feature branch to development. If the PR contains a single commit, the commit message should conform to [Conventional Commits](http://conventionalcommits.org/en/v1.0.0/). Should the PR contain _multiple_ commits, its title should conform to [Convention Commits](http://conventionalcommits.org/en/v1.0.0/).
2. Merges to `main` trigger deployments to the Proxy development cluster.
3.

<details>

<summary>Manual Deployment Steps</summary>

> [!WARNING]
> This should only be necessary is extreme circumstances. Use of automated deployments via GitHub Workflows is preferred.

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
