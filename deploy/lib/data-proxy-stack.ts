import * as cdk from "aws-cdk-lib";
import { aws_ec2 as ec2 } from "aws-cdk-lib";
import { Construct } from "constructs";
import { VercelApiProxy } from "./vercel-api-proxy";

export class DataProxyStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    const vpc = ec2.Vpc.fromLookup(this, "Vpc", {
      vpcId: "vpc-05858c6e5697bbc40",
    });

    new VercelApiProxy(this, "EgressProxy", {
      vpc,
      proxyDomain: "vercel-api.internal",
    });
  }
}
