import * as cdk from "aws-cdk-lib";
import {
  aws_ec2 as ec2,
  aws_iam as iam,
  aws_route53 as route53,
  aws_route53_targets as route53_targets,
} from "aws-cdk-lib";
import { Construct } from "constructs";

interface VercelApiProxyProps {
  vpc: ec2.IVpc;
  proxyDomain: string;
}

export class VercelApiProxy extends Construct {
  public readonly url: string;
  /**
   * To work around Vercel's firewall, we must proxy all requests for the Proxy API through
   * a Squid proxy. This will allow us to have a stable IP address for the Proxy API which
   * we can add to the Vercel firewall's bypass list. This allows us to retain ephemeral IP
   * addresses for the Proxy API and to avoid using other techniques like passing data
   * through a NAT Gateway which would have considerable cost implications.
   */
  constructor(scope: Construct, id: string, props: VercelApiProxyProps) {
    super(scope, id);

    const proxyPort = 3128;

    // Create security group for the proxy
    const proxySg = new ec2.SecurityGroup(this, "proxy-sg", {
      vpc: props.vpc,
      description: "Allow inbound from ECS for Squid proxy",
      allowAllOutbound: true,
    });

    // Allow ECS (internal) traffic on port 3128
    proxySg.addIngressRule(
      ec2.Peer.ipv4(props.vpc.vpcCidrBlock),
      ec2.Port.tcp(proxyPort),
      "Allow ECS to connect to Squid"
    );

    // Squid install and minimal config
    const userData = ec2.UserData.forLinux();
    userData.addCommands(
      "yum update -y",
      "yum install -y squid",

      // Write squid.conf using heredoc
      "cat <<'EOF' > /etc/squid/squid.conf",
      `http_port ${proxyPort}`,
      "acl all src 0.0.0.0/0",
      "http_access allow all",
      "EOF",

      // Enable and start Squid
      "systemctl enable squid",
      "systemctl restart squid"
    );

    // Enable SSM access for the EC2 instance
    const ssmRole = new iam.Role(this, "ec2-ssm-role", {
      assumedBy: new iam.ServicePrincipal("ec2.amazonaws.com"),
      managedPolicies: [
        iam.ManagedPolicy.fromAwsManagedPolicyName(
          "AmazonSSMManagedInstanceCore"
        ),
      ],
    });

    // Launch EC2 instance
    const instance = new ec2.Instance(this, "squid-proxy", {
      vpc: props.vpc,
      role: ssmRole,
      instanceType: ec2.InstanceType.of(
        ec2.InstanceClass.T3,
        ec2.InstanceSize.MICRO
      ),
      machineImage: ec2.MachineImage.latestAmazonLinux2023(),
      vpcSubnets: { subnetType: ec2.SubnetType.PUBLIC },
      securityGroup: proxySg,
      userData,
    });

    // Allocate and associate Elastic IP
    const eip = new ec2.CfnEIP(this, "proxy-eip", {
      domain: "vpc",
      tags: [
        {
          key: "Name",
          value: `${cdk.Stack.of(this).stackName}-proxy-eip`,
        },
      ],
    });
    new ec2.CfnEIPAssociation(this, "proxy-eip-assoc", {
      allocationId: eip.attrAllocationId,
      instanceId: instance.instanceId,
    });

    // Route 53 Private Hosted Zone
    const zone = new route53.PrivateHostedZone(this, "proxy-zone", {
      vpc: props.vpc,
      zoneName: props.proxyDomain,
    });
    new route53.ARecord(this, "proxy-a-record", {
      zone,
      target: route53.RecordTarget.fromIpAddresses(instance.instancePrivateIp),
      ttl: cdk.Duration.seconds(60),
    });

    this.url = `http://${props.proxyDomain}:${proxyPort}`;
  }
}
