resource "random_id" "identity_signing_key" {
  byte_length = 32
}

resource "aws_ssm_parameter" "identity_signing_key" {
  name  = "/hive/test/fargate-app/identity-signing-key"
  type  = "SecureString"
  value = random_id.identity_signing_key.hex
}

resource "aws_cloudwatch_log_group" "hive_service" {
  name              = "/ecs/hive-service"
  retention_in_days = 7
}

resource "aws_ecs_cluster" "this" {
  name = "hive-test"
}

locals {
  # PrivateLink connection hostname: <cluster-id>.<service-identifier>.<region>.on.aws. The service
  # name is "com.amazonaws.<region>.<service-identifier>" (e.g. "com.amazonaws.eu-west-1.dsql-2syq");
  # the last dot-separated segment is the service identifier the hostname needs.
  dsql_service_name_parts = split(".", data.terraform_remote_state.dsql_test.outputs.vpc_endpoint_service_name)
  dsql_service_identifier = local.dsql_service_name_parts[length(local.dsql_service_name_parts) - 1]
  dsql_privatelink_host   = "${data.terraform_remote_state.dsql_test.outputs.identifier}.${local.dsql_service_identifier}.eu-west-1.on.aws"
  dsql_jdbc_url           = "jdbc:aws-dsql:postgresql://${local.dsql_privatelink_host}/postgres"
}

resource "aws_ecs_task_definition" "hive_service" {
  family                   = "hive-service"
  requires_compatibilities  = ["FARGATE"]
  network_mode              = "awsvpc"
  cpu                        = "1024"
  memory                     = "2048"
  execution_role_arn         = aws_iam_role.task_execution.arn
  task_role_arn              = aws_iam_role.task.arn

  runtime_platform {
    cpu_architecture        = "ARM64"
    operating_system_family = "LINUX"
  }

  container_definitions = jsonencode([
    {
      name      = "hive-service"
      image     = "${aws_ecr_repository.hive_service.repository_url}:latest"
      essential = true
      portMappings = [{
        containerPort = 8080
        protocol      = "tcp"
      }]
      environment = [
        { name = "HIVE_DATABASE_URL", value = local.dsql_jdbc_url },
        { name = "HIVE_DATABASE_USER", value = "admin" },
      ]
      secrets = [{
        name      = "HIVE_IDENTITY_SIGNING_KEY"
        valueFrom = aws_ssm_parameter.identity_signing_key.arn
      }]
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          "awslogs-group"         = aws_cloudwatch_log_group.hive_service.name
          "awslogs-region"        = "eu-west-1"
          "awslogs-stream-prefix" = "ecs"
        }
      }
    }
  ])
}

resource "aws_ecs_service" "hive_service" {
  name            = "hive-service"
  cluster         = aws_ecs_cluster.this.id
  task_definition = aws_ecs_task_definition.hive_service.arn
  desired_count   = 1
  launch_type     = "FARGATE"

  network_configuration {
    subnets          = aws_subnet.private[*].id
    security_groups  = [aws_security_group.service.id]
    assign_public_ip = false
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.hive_service.arn
    container_name   = "hive-service"
    container_port   = 8080
  }

  depends_on = [aws_lb_listener.http, aws_vpc_endpoint.ecr_api, aws_vpc_endpoint.ecr_dkr, aws_vpc_endpoint.logs, aws_vpc_endpoint.dsql, aws_vpc_endpoint.s3, aws_vpc_endpoint.ssm]
}
