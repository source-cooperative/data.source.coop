import * as cdk from "aws-cdk-lib";
import { aws_ec2 as ec2 } from "aws-cdk-lib";
import { Construct } from "constructs";
import { VercelApiProxy } from "./vercel-api-proxy";
import { SourceDataProxy } from "./source-data-proxy";
import { VpcEndpoints } from "./vpc-endpoints";

interface DataProxyStackProps extends cdk.StackProps {
  vpcId: string;
  proxyDomain: string;
  sourceApiUrl: string;
  proxyDesiredCount: number;
  certificateArn: string;
}

export class DataProxyStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: DataProxyStackProps) {
    super(scope, id, props);

    const vpc = ec2.Vpc.fromLookup(this, "vpc", { vpcId: props.vpcId });

    // Create Vercel API proxy (existing functionality)
    const vercelApiProxy = new VercelApiProxy(this, "vercel-api-proxy", {
      vpc,
      proxyDomain: props.proxyDomain,
    });

    new SourceDataProxy(this, "source-data-proxy", {
      vpc,
      environment: {
        RUST_LOG: "info",
        SOURCE_API_PROXY_URL: vercelApiProxy.url,
        SOURCE_API_URL: props.sourceApiUrl,
      },
      desiredCount: props.proxyDesiredCount,
      certificateArn: props.certificateArn,
    });
  }
}
