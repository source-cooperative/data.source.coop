import * as cdk from "aws-cdk-lib";
import {
  aws_ec2 as ec2,
  aws_ecs as ecs,
  aws_ecs_patterns as ecs_patterns,
  aws_logs as logs,
  aws_secretsmanager as secretsmanager,
  aws_elasticloadbalancingv2 as elbv2,
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
        vpc: props.vpc,
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
          environment: props.environment,
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
      }
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
