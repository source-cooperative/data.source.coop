import * as cdk from "aws-cdk-lib";
import {
  aws_ecs as ecs,
  aws_logs as logs,
  aws_iam as iam,
} from "aws-cdk-lib";
import { Construct } from "constructs";

interface AdotCollectorProps {
  taskDefinition: ecs.TaskDefinition;
  /**
   * CPU units to allocate to the ADOT collector (default: 256 = 0.25 vCPU)
   */
  cpu?: number;
  /**
   * Memory limit in MiB for the ADOT collector (default: 512 MB)
   */
  memoryLimitMiB?: number;
  /**
   * Log retention period (default: ONE_WEEK)
   */
  logRetention?: logs.RetentionDays;
}

/**
 * AWS Distro for OpenTelemetry (ADOT) Collector sidecar container.
 *
 * This construct adds an ADOT collector sidecar to an ECS task definition.
 * The collector receives OpenTelemetry data from application containers via OTLP
 * and forwards it to AWS X-Ray for distributed tracing and CloudWatch for metrics.
 *
 * ## Usage
 *
 * Applications should send traces to:
 * - OTLP gRPC: `http://localhost:4317`
 * - OTLP HTTP: `http://localhost:4318`
 *
 * ## Configuration
 *
 * The collector uses the default ECS configuration (`/etc/ecs/ecs-default-config.yaml`)
 * which includes:
 * - OTLP receiver on ports 4317 (gRPC) and 4318 (HTTP)
 * - AWS X-Ray exporter for distributed tracing
 * - CloudWatch EMF exporter for metrics
 *
 * ## IAM Permissions
 *
 * The task role is automatically granted permissions to:
 * - Send traces to X-Ray
 * - Publish metrics to CloudWatch
 * - Write logs to CloudWatch Logs
 *
 * @see https://aws-otel.github.io/docs/getting-started/ecs
 */
export class AdotCollector extends Construct {
  public readonly container: ecs.ContainerDefinition;

  constructor(scope: Construct, id: string, props: AdotCollectorProps) {
    super(scope, id);

    const stack = cdk.Stack.of(this);

    // Add ADOT collector container to the task definition
    this.container = props.taskDefinition.addContainer("aws-otel-collector", {
      image: ecs.ContainerImage.fromRegistry(
        "public.ecr.aws/aws-observability/aws-otel-collector:latest"
      ),
      cpu: props.cpu ?? 256,
      memoryLimitMiB: props.memoryLimitMiB ?? 512,
      essential: true,
      command: ["--config=/etc/ecs/ecs-default-config.yaml"],
      logging: ecs.LogDrivers.awsLogs({
        streamPrefix: "adot-collector",
        logGroup: new logs.LogGroup(this, "log-group", {
          logGroupName: `/ecs/${stack.stackName}-adot-collector`,
          retention: props.logRetention ?? logs.RetentionDays.ONE_WEEK,
        }),
        mode: ecs.AwsLogDriverMode.NON_BLOCKING,
      }),
      environment: {
        AWS_REGION: stack.region,
      },
    });

    // Add port mappings for OTLP receivers
    this.container.addPortMappings(
      {
        containerPort: 4317,
        protocol: ecs.Protocol.TCP,
        name: "otlp-grpc",
      },
      {
        containerPort: 4318,
        protocol: ecs.Protocol.TCP,
        name: "otlp-http",
      }
    );

    // Grant permissions to send traces to X-Ray and metrics to CloudWatch
    if (props.taskDefinition.taskRole) {
      props.taskDefinition.taskRole.addToPrincipalPolicy(
        new iam.PolicyStatement({
          effect: iam.Effect.ALLOW,
          actions: [
            // X-Ray permissions
            "xray:PutTraceSegments",
            "xray:PutTelemetryRecords",
            // CloudWatch permissions
            "cloudwatch:PutMetricData",
            // CloudWatch Logs permissions (for the collector itself)
            "logs:PutLogEvents",
            "logs:CreateLogGroup",
            "logs:CreateLogStream",
            "logs:DescribeLogStreams",
            "logs:DescribeLogGroups",
          ],
          resources: ["*"],
        })
      );
    }
  }
}
