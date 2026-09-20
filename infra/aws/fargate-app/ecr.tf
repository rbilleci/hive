resource "aws_ecr_repository" "hive_service" {
  name                 = "hive-service"
  image_tag_mutability = "MUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }

  tags = {
    Purpose = "hive-service container images for the non-prod Fargate validation deployment"
  }
}

resource "aws_ecr_lifecycle_policy" "hive_service" {
  repository = aws_ecr_repository.hive_service.name

  policy = jsonencode({
    rules = [{
      rulePriority = 1
      description  = "Expire untagged images after 3 days"
      selection = {
        tagStatus   = "untagged"
        countType   = "sinceImagePushed"
        countUnit   = "days"
        countNumber = 3
      }
      action = { type = "expire" }
    }]
  })
}
