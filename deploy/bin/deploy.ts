#!/usr/bin/env node
import * as cdk from "aws-cdk-lib";
import { DataProxyStack } from "../lib/data-proxy-stack";
import { Tags } from "aws-cdk-lib";

const stage = process.env.STAGE || "dev";
const vpcId = process.env.VPC_ID;
if (!vpcId) {
  throw new Error("VPC_ID is not set");
}
const certificateArn = process.env.CERTIFICATE_ARN;
if (!certificateArn) {
  throw new Error("CERTIFICATE_ARN is not set");
}
const taskCount = process.env.TASK_COUNT || 1;
const sourceApiUrl = process.env.SOURCE_API_URL || "https://s2.source.coop";

const app = new cdk.App();
const stack = new DataProxyStack(app, `DataProxy-${stage}`, {
  vpcId,
  proxyDomain: `vercel-api-${stage}.internal`,
  proxyDesiredCount: Number(taskCount),
  sourceApiUrl,
  env: {
    account: process.env.AWS_ACCOUNT_ID,
    region: process.env.AWS_REGION,
  },
  certificateArn,
});

Tags.of(stack).add("Cfn-Stack", stack.stackName, {
  applyToLaunchedInstances: true,
});
