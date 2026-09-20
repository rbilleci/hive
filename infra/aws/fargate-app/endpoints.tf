# One AZ only, to minimize interface-endpoint hourly cost: an endpoint's ENI is reachable from both
# private subnets over the VPC's own local route regardless of which subnet it sits in.

resource "aws_vpc_endpoint" "ecr_api" {
  vpc_id              = aws_vpc.this.id
  service_name        = "com.amazonaws.eu-west-1.ecr.api"
  vpc_endpoint_type   = "Interface"
  subnet_ids          = [aws_subnet.private[0].id]
  security_group_ids  = [aws_security_group.endpoints.id]
  private_dns_enabled = true

  tags = { Name = "hive-fargate-test-ecr-api" }
}

resource "aws_vpc_endpoint" "ecr_dkr" {
  vpc_id              = aws_vpc.this.id
  service_name        = "com.amazonaws.eu-west-1.ecr.dkr"
  vpc_endpoint_type   = "Interface"
  subnet_ids          = [aws_subnet.private[0].id]
  security_group_ids  = [aws_security_group.endpoints.id]
  private_dns_enabled = true

  tags = { Name = "hive-fargate-test-ecr-dkr" }
}

resource "aws_vpc_endpoint" "logs" {
  vpc_id              = aws_vpc.this.id
  service_name        = "com.amazonaws.eu-west-1.logs"
  vpc_endpoint_type   = "Interface"
  subnet_ids          = [aws_subnet.private[0].id]
  security_group_ids  = [aws_security_group.endpoints.id]
  private_dns_enabled = true

  tags = { Name = "hive-fargate-test-logs" }
}

# The ECS agent itself (not the application) calls this to resolve the task's `secrets` block
# (the identity-signing-key SSM parameter) before the container starts - missed on the first pass,
# surfaced as "ResourceInitializationError: unable to pull secrets... from AWS Systems Manager".
resource "aws_vpc_endpoint" "ssm" {
  vpc_id              = aws_vpc.this.id
  service_name        = "com.amazonaws.eu-west-1.ssm"
  vpc_endpoint_type   = "Interface"
  subnet_ids          = [aws_subnet.private[0].id]
  security_group_ids  = [aws_security_group.endpoints.id]
  private_dns_enabled = true

  tags = { Name = "hive-fargate-test-ssm" }
}

# Aurora DSQL's PrivateLink *connection* endpoint (distinct from its per-region *management*
# endpoint, which is only for cluster create/list/delete admin API calls this task never makes).
# Private DNS makes the standard <cluster-id>.<service-identifier>.<region>.on.aws hostname resolve
# to this endpoint from inside the VPC instead of the public endpoint.
resource "aws_vpc_endpoint" "dsql" {
  vpc_id              = aws_vpc.this.id
  service_name        = data.terraform_remote_state.dsql_test.outputs.vpc_endpoint_service_name
  vpc_endpoint_type   = "Interface"
  subnet_ids          = [aws_subnet.private[0].id]
  security_group_ids  = [aws_security_group.endpoints.id]
  private_dns_enabled = true

  tags = { Name = "hive-fargate-test-dsql" }
}

# Gateway endpoint (no hourly cost): ECR image-layer pulls are redirected to S3.
resource "aws_vpc_endpoint" "s3" {
  vpc_id            = aws_vpc.this.id
  service_name      = "com.amazonaws.eu-west-1.s3"
  vpc_endpoint_type = "Gateway"
  route_table_ids   = [aws_route_table.private.id]

  tags = { Name = "hive-fargate-test-s3" }
}
