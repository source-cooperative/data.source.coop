import * as cdk from "aws-cdk-lib";
import {
  aws_ec2 as ec2,
  aws_ecs as ecs,
  aws_ecs_patterns as ecs_patterns,
  aws_logs as logs,
  aws_secretsmanager as secretsmanager,
  aws_elasticloadbalancingv2 as elbv2,
  aws_iam as iam,
} from "aws-cdk-lib";
import { Certificate } from "aws-cdk-lib/aws-certificatemanager";
import { Construct } from "constructs";

interface SourceDataProxyProps {
  vpc: ec2.IVpc;
  desiredCount: number;
  environment: Record<string, string>;
  certificateArn: string;
}

export class SourceDataProxy extends Construct {
  public readonly service: ecs_patterns.ApplicationLoadBalancedFargateService;

  constructor(scope: Construct, id: string, props: SourceDataProxyProps) {
    super(scope, id);

    const stack = cdk.Stack.of(this);

    const cluster = new ecs.Cluster(this, "cluster", {
      clusterName: `${stack.stackName}-cluster`,
      vpc: props.vpc,
      enableFargateCapacityProviders: true,
      containerInsightsV2: ecs.ContainerInsights.ENHANCED,
    });

    const sourceApiKeySecret = new secretsmanager.Secret(
      this,
      "source-api-key",
      {
        secretName: `${stack.stackName}-source-api-key`,
        description:
          "API Key used to make authenticated requests to the Source API on Vercel",
      }
    );

    // Create Application Load Balanced Fargate Service using the pattern
    this.service = new ecs_patterns.ApplicationLoadBalancedFargateService(
      this,
      "service",
      {
        serviceName: `${stack.stackName}-proxy`,
        cluster,
        cpu: 4 * 1024, // 4 vCPU
        desiredCount: props.desiredCount,
        memoryLimitMiB: 12 * 1024, // 12 GB
        taskImageOptions: {
          image: ecs.ContainerImage.fromAsset("../", {
            buildArgs: {
              BUILDPLATFORM: "linux/amd64",
              TARGETPLATFORM: "linux/amd64",
            },
          }),
          containerPort: 8080,
          family: `${stack.stackName}-proxy`,
          environment: {
            ...props.environment,
            OTEL_EXPORTER_OTLP_ENDPOINT: "http://localhost:4317",
          },
          secrets: {
            SOURCE_API_KEY: ecs.Secret.fromSecretsManager(sourceApiKeySecret),
          },
          logDriver: ecs.LogDrivers.awsLogs({
            streamPrefix: "ecs",
            logGroup: new logs.LogGroup(this, "log-group", {
              logGroupName: `/ecs/${stack.stackName}-proxy`,
              retention: logs.RetentionDays.ONE_MONTH,
            }),
            mode: ecs.AwsLogDriverMode.NON_BLOCKING,
            maxBufferSize: cdk.Size.mebibytes(25),
          }),
        },
        runtimePlatform: {
          cpuArchitecture: ecs.CpuArchitecture.X86_64,
          operatingSystemFamily: ecs.OperatingSystemFamily.LINUX,
        },
        publicLoadBalancer: true,
        loadBalancerName: `${stack.stackName}-alb`,
        protocol: elbv2.ApplicationProtocol.HTTPS,
        listenerPort: 443,
        certificate: Certificate.fromCertificateArn(
          this,
          "certificate",
          props.certificateArn
        ),
        enableExecuteCommand: true,
        circuitBreaker: { rollback: true },
        assignPublicIp: true,
        capacityProviderStrategies: [
          {
            capacityProvider: "FARGATE_SPOT",
            // Prefer spot instances over on-demand instances
            weight: 2,
          },
          {
            capacityProvider: "FARGATE",
            // Use on-demand instances as a fallback
            weight: 1,
          },
        ],
      }
    );

    // Add ADOT collector sidecar
    this.service.taskDefinition.addContainer('aws-otel-collector', {
      image: ecs.ContainerImage.fromRegistry(
        'public.ecr.aws/aws-observability/aws-otel-collector:latest'
      ),
      logging: ecs.LogDrivers.awsLogs({
        streamPrefix: 'adot',
        logGroup: new logs.LogGroup(this, 'adot-log-group', {
          logGroupName: `/ecs/${stack.stackName}-adot`,
          retention: logs.RetentionDays.ONE_WEEK,
        }),
      }),
      environment: {
        AOT_CONFIG_CONTENT: JSON.stringify({
          receivers: {
            otlp: {
              protocols: {
                grpc: { endpoint: '0.0.0.0:4317' },
              },
            },
          },
          processors: { batch: {} },
          exporters: {
            awsxray: { region: stack.region },
          },
          service: {
            pipelines: {
              traces: {
                receivers: ['otlp'],
                processors: ['batch'],
                exporters: ['awsxray'],
              },
            },
          },
        }),
      },
      memoryReservationMiB: 512,
      cpu: 256,
    });

    // Add X-Ray permissions to task role
    this.service.taskDefinition.addToTaskRolePolicy(
      new iam.PolicyStatement({
        actions: ['xray:PutTraceSegments', 'xray:PutTelemetryRecords'],
        resources: ['*'],
      })
    );

    if (this.service.taskDefinition.executionRole) {
      sourceApiKeySecret.grantRead(this.service.taskDefinition.executionRole);
    }

    // Output the ALB DNS name
    new cdk.CfnOutput(this, "alb-dns", {
      value: this.service.loadBalancer.loadBalancerDnsName,
      description: "Application Load Balancer DNS name",
      exportName: `${cdk.Stack.of(this).stackName}-alb-dns`,
    });

    // Output the service name
    new cdk.CfnOutput(this, "service-name", {
      value: this.service.service.serviceName,
      description: "ECS Service name",
      exportName: `${cdk.Stack.of(this).stackName}-service-name`,
    });
  }
}
