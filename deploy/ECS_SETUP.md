# ECS Fargate Setup with Application Load Balancer

This document describes the CDK infrastructure for the Source Data Proxy ECS Fargate service.

## Architecture

The setup includes:

1. **ECS Cluster**: Hosts the Fargate services
2. **Application Load Balancer**: Routes traffic to the ECS service
3. **ECS Fargate Service**: Runs the source-data-proxy container
4. **Target Group**: Health checks and load balancing configuration
5. **Security Groups**: Network access control

## Components

### EcsCluster (`ecs-cluster.ts`)
- Creates an ECS cluster with container insights and service discovery
- Enables ECS Exec for debugging
- Configures CloudWatch logging

### SourceDataProxy (`source-data-proxy.ts`)
- Uses the `ApplicationLoadBalancedFargateService` pattern for simplified setup
- Automatically creates ALB, target group, and service configuration
- Defines ECS task definition with Fargate configuration
- Creates the ECS service with auto-scaling capabilities
- Builds Docker image from source code using `ContainerImage.fromAsset()`

## Configuration

### Environment Variables
- `TASK_ROLE_ARN`: IAM role for the ECS task
- `EXECUTION_ROLE_ARN`: IAM role for ECS task execution

### Task Definition
- **CPU**: 4 vCPU (4096 CPU units)
- **Memory**: 12 GB (12288 MB)
- **Architecture**: x86_64 Linux
- **Network Mode**: awsvpc
- **Port**: 8080 (HTTP)

### Load Balancer
- **Type**: Application Load Balancer (automatically configured)
- **Protocol**: HTTP (port 80)
- **Health Check**: Default ALB health checks
- **Target Type**: IP (for Fargate)

## Deployment

The service is deployed through CDK with the following workflow:

1. **CDK Deploy**: Infrastructure is deployed using CDK, which automatically builds the Docker image from source code
2. **Service Update**: ECS service is updated with new task definition

## Security

- ECS service runs in private subnets
- ALB is internet-facing but only forwards to private ECS tasks
- Security groups restrict access appropriately
- IAM roles provide least-privilege access

## Monitoring

- CloudWatch logs for container logs
- Container insights for cluster monitoring
- ALB access logs for traffic analysis
- Health checks for service availability

## Scaling

- Service starts with 2 desired tasks for high availability
- Circuit breaker enabled for automatic rollback on failures
- Service discovery enabled for internal communication

## Integration

The service integrates with:
- Vercel API proxy for external API access
- Source API for data retrieval
- CloudWatch for logging and monitoring
