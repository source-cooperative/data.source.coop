#!/usr/bin/env node
import * as cdk from "aws-cdk-lib";
import { DataProxyStack } from "../lib/data-proxy-stack";

const stage = process.env.STAGE || "dev";

const vpcId = process.env.VPC_ID;
if (!vpcId) {
  throw new Error("VPC_ID is not set");
}

const app = new cdk.App();
new DataProxyStack(app, `DataProxy-${stage}`, {
  vpcId,
  proxyDomain: `vercel-api-${stage}.internal`,
  env: {
    account: process.env.AWS_ACCOUNT_ID,
    region: process.env.AWS_REGION,
  },
});
