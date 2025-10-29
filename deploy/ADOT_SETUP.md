# AWS Distro for OpenTelemetry (ADOT) Setup

This deployment includes AWS Distro for OpenTelemetry (ADOT) Collector as a sidecar container for distributed tracing with AWS X-Ray and metrics collection with CloudWatch.

## Architecture

```
┌─────────────────────────────────────────────────┐
│              ECS Task                           │
│                                                 │
│  ┌──────────────────┐    ┌─────────────────┐  │
│  │                  │    │                 │  │
│  │  Application     │───▶│  ADOT Collector │  │
│  │  Container       │    │                 │  │
│  │                  │    │  localhost:4317 │  │
│  └──────────────────┘    └────────┬────────┘  │
│                                    │           │
└────────────────────────────────────┼───────────┘
                                     │
                      ┌──────────────┴───────────────┐
                      │                              │
                      ▼                              ▼
              ┌───────────────┐            ┌─────────────────┐
              │  AWS X-Ray    │            │  CloudWatch     │
              │  (Traces)     │            │  (Metrics/Logs) │
              └───────────────┘            └─────────────────┘
```

## Components

### Application Container (source-data-proxy)

The main Rust application is instrumented with OpenTelemetry and sends traces to the ADOT collector via OTLP:

- **Endpoint**: `http://localhost:4317` (OTLP gRPC)
- **Protocol**: OpenTelemetry Protocol (OTLP)
- **Context Propagation**: W3C TraceContext headers
- **Sampling**: 10% (configurable via `OTEL_TRACE_SAMPLE_RATE`)

### ADOT Collector Sidecar

The ADOT collector runs as a sidecar container in the same ECS task:

- **Image**: `public.ecr.aws/aws-observability/aws-otel-collector:latest`
- **CPU**: 256 units (0.25 vCPU)
- **Memory**: 512 MB
- **Configuration**: Default ECS configuration (`/etc/ecs/ecs-default-config.yaml`)

#### Receivers

- **OTLP gRPC**: Port 4317
- **OTLP HTTP**: Port 4318

#### Exporters

- **AWS X-Ray**: For distributed tracing
- **CloudWatch EMF**: For metrics

## Environment Variables

### Application Container

```bash
# OpenTelemetry Configuration
OTEL_SERVICE_NAME=source-data-proxy
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
OTEL_TRACE_SAMPLE_RATE=0.1
OTEL_TRACES_SAMPLER=parentbased_traceidratio
RUST_LOG=info,source_data_proxy=debug
TRACING_SKIP_PATHS=/
DEPLOYMENT_ENV=<stack-name>

# Disable telemetry for local development (optional)
OTEL_SDK_DISABLED=true
```

### ADOT Collector

```bash
AWS_REGION=<region>
```

## IAM Permissions

The ECS task role is granted the following permissions:

```json
{
  "Effect": "Allow",
  "Action": [
    "xray:PutTraceSegments",
    "xray:PutTelemetryRecords",
    "cloudwatch:PutMetricData",
    "logs:PutLogEvents",
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:DescribeLogStreams",
    "logs:DescribeLogGroups"
  ],
  "Resource": "*"
}
```

## Viewing Traces

### AWS X-Ray Console

1. Navigate to AWS X-Ray Console
2. Select "Service Map" to see the distributed trace topology
3. Select "Traces" to view individual traces
4. Filter by service name: `source-data-proxy`

### CloudWatch Logs

Application logs are written to:
- **Log Group**: `/ecs/<stack-name>-proxy`
- **Format**: JSON with OpenTelemetry context (trace_id, span_id)

ADOT Collector logs are written to:
- **Log Group**: `/ecs/<stack-name>-adot-collector`

## Resource Usage

### Default Configuration

- **Application Container**: 4 vCPU, 12 GB RAM
- **ADOT Collector**: 0.25 vCPU, 512 MB RAM
- **Total per task**: 4.25 vCPU, 12.5 GB RAM

## Customization

To customize the ADOT collector configuration, modify the `AdotCollector` construct:

```typescript
new AdotCollector(this, "adot-collector", {
  taskDefinition: this.service.taskDefinition,
  cpu: 512,                              // Increase CPU
  memoryLimitMiB: 1024,                  // Increase memory
  logRetention: logs.RetentionDays.ONE_MONTH, // Longer retention
});
```

## Cost Optimization

### Sampling

Adjust the sampling rate to control costs:

```typescript
environment: {
  OTEL_TRACE_SAMPLE_RATE: "0.01", // 1% sampling
}
```

### Health Check Filtering

Health checks are automatically filtered from tracing via `TRACING_SKIP_PATHS=/` to reduce noise and cost.

### ADOT Collector Resources

The default allocation (0.25 vCPU, 512 MB) is suitable for moderate traffic. Monitor CloudWatch metrics and adjust if needed.

## Troubleshooting

### No Traces in X-Ray

1. Check ADOT collector logs: `/ecs/<stack-name>-adot-collector`
2. Verify IAM permissions on the task role
3. Check application logs for initialization errors
4. Verify OTLP endpoint is set to `http://localhost:4317`

### High CPU/Memory Usage on ADOT Collector

1. Check the number of spans being sent
2. Consider reducing sampling rate
3. Increase ADOT collector resources
4. Review batch configuration in ADOT config

### Local Development

For local development without ADOT:

```bash
# Disable OpenTelemetry entirely
OTEL_SDK_DISABLED=true cargo run

# OR run a local OTLP collector
docker run -p 4317:4317 -p 16686:16686 jaegertracing/all-in-one:latest
cargo run
```

## References

- [AWS ADOT Documentation](https://aws-otel.github.io/)
- [ADOT on ECS](https://aws-otel.github.io/docs/getting-started/ecs)
- [AWS X-Ray Developer Guide](https://docs.aws.amazon.com/xray/latest/devguide/)
- [OpenTelemetry Rust](https://opentelemetry.io/docs/instrumentation/rust/)
