data "aws_iam_policy_document" "ecs_tasks_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
  }
}

# Execution role: what ECS itself needs to start the task (pull the image, write logs, read the
# signing-key parameter). Distinct from the task role below, which is what the application uses at
# runtime.
resource "aws_iam_role" "task_execution" {
  name               = "hive-fargate-test-task-execution"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_assume.json
}

resource "aws_iam_role_policy_attachment" "task_execution_managed" {
  role       = aws_iam_role.task_execution.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

data "aws_iam_policy_document" "task_execution_ssm" {
  statement {
    actions   = ["ssm:GetParameters"]
    resources = [aws_ssm_parameter.identity_signing_key.arn]
  }
}

resource "aws_iam_role_policy" "task_execution_ssm" {
  name   = "read-identity-signing-key"
  role   = aws_iam_role.task_execution.id
  policy = data.aws_iam_policy_document.task_execution_ssm.json
}

# Task role: the application's own runtime permissions. Only Aurora DSQL IAM-token connect today.
resource "aws_iam_role" "task" {
  name               = "hive-fargate-test-task"
  assume_role_policy = data.aws_iam_policy_document.ecs_tasks_assume.json
}

data "aws_iam_policy_document" "task_dsql_connect" {
  statement {
    actions   = ["dsql:DbConnectAdmin"]
    resources = [data.terraform_remote_state.dsql_test.outputs.arn]
  }
}

resource "aws_iam_role_policy" "task_dsql_connect" {
  name   = "dsql-connect-admin"
  role   = aws_iam_role.task.id
  policy = data.aws_iam_policy_document.task_dsql_connect.json
}
