import * as cdk from "aws-cdk-lib";
import { aws_ec2 as ec2 } from "aws-cdk-lib";
import { Construct } from "constructs";
import { VercelApiProxy } from "./vercel-api-proxy";

interface DataProxyStackProps extends cdk.StackProps {
  vpcId: string;
  proxyDomain: string;
}

export class DataProxyStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: DataProxyStackProps) {
    super(scope, id, props);

    const vpc = ec2.Vpc.fromLookup(this, "vpc", { vpcId: props.vpcId });

    new VercelApiProxy(this, "vercel-api-proxy", {
      vpc,
      proxyDomain: props.proxyDomain,
    });
  }
}
