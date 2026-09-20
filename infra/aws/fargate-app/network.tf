# Fully private: no internet gateway, no NAT gateway, no public subnet anywhere in this VPC. The
# task, and the internal ALB in front of it, reach ECR, CloudWatch Logs, and Aurora DSQL only
# through the VPC endpoints in endpoints.tf. Two private subnets for AZ redundancy of the task and
# the ALB; the endpoints themselves live in only one subnet to minimize interface-endpoint cost
# (any subnet in the VPC can still reach them over the VPC's local route).

resource "aws_vpc" "this" {
  cidr_block           = "10.20.0.0/16"
  enable_dns_support   = true
  enable_dns_hostnames = true

  tags = {
    Name = "hive-fargate-test"
  }
}

resource "aws_subnet" "private" {
  count             = 2
  vpc_id            = aws_vpc.this.id
  cidr_block        = "10.20.${count.index}.0/24"
  availability_zone = data.aws_availability_zones.available.names[count.index]

  tags = {
    Name = "hive-fargate-test-private-${count.index}"
  }
}

resource "aws_route_table" "private" {
  vpc_id = aws_vpc.this.id

  tags = {
    Name = "hive-fargate-test-private"
  }
}

resource "aws_route_table_association" "private" {
  count          = 2
  subnet_id      = aws_subnet.private[count.index].id
  route_table_id = aws_route_table.private.id
}

resource "aws_security_group" "alb" {
  name        = "hive-fargate-test-alb"
  description = "Internal ALB ingress for the hive-service validation deployment, VPC-only."
  vpc_id      = aws_vpc.this.id

  ingress {
    description = "HTTP from within the VPC only"
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = [aws_vpc.this.cidr_block]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = [aws_vpc.this.cidr_block]
  }

  tags = {
    Name = "hive-fargate-test-alb"
  }
}

resource "aws_security_group" "service" {
  name        = "hive-fargate-test-service"
  description = "hive-service Fargate task: accepts only ALB traffic, reaches only the VPC endpoints."
  vpc_id      = aws_vpc.this.id

  ingress {
    description     = "Container port from the ALB only"
    from_port       = 8080
    to_port         = 8080
    protocol        = "tcp"
    security_groups = [aws_security_group.alb.id]
  }

  egress {
    description = "To the interface VPC endpoints (ECR, CloudWatch Logs, DSQL) - no internet route exists anyway"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = [aws_vpc.this.cidr_block]
  }

  egress {
    description     = "To the S3 gateway endpoint for ECR image layers - a gateway endpoint keeps the S3 service public IP range as the packet destination even though the route table diverts it off the internet, so this is not covered by the VPC-CIDR rule above"
    from_port       = 443
    to_port         = 443
    protocol        = "tcp"
    prefix_list_ids = [aws_vpc_endpoint.s3.prefix_list_id]
  }

  tags = {
    Name = "hive-fargate-test-service"
  }
}

resource "aws_security_group" "endpoints" {
  name        = "hive-fargate-test-endpoints"
  description = "Shared interface VPC endpoints: ECR api/dkr, CloudWatch Logs, Aurora DSQL connection."
  vpc_id      = aws_vpc.this.id

  ingress {
    description     = "HTTPS (ECR, CloudWatch Logs) from the service"
    from_port       = 443
    to_port         = 443
    protocol        = "tcp"
    security_groups = [aws_security_group.service.id]
  }

  ingress {
    description     = "PostgreSQL wire protocol (Aurora DSQL PrivateLink connection endpoint) from the service"
    from_port       = 5432
    to_port         = 5432
    protocol        = "tcp"
    security_groups = [aws_security_group.service.id]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = [aws_vpc.this.cidr_block]
  }

  tags = {
    Name = "hive-fargate-test-endpoints"
  }
}
