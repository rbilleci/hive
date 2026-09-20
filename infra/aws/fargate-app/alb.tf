# HTTP only, no ACM certificate or Okta OIDC listener rule: this validates that the Fargate
# deployment path works end to end, not the production ingress design in
# docs/architecture-specification-v1.1.md#management-plane-ingress. Do not treat this ALB as a
# stand-in for the OIDC-authenticated production listener.
#
# internal = true: there is no public subnet in this VPC for an internet-facing ALB to use. Reach
# it from within the VPC (a bastion, another task, VPC Reachability Analyzer) - not from a laptop
# over the public internet. Validate the deployment via ECS/target-group health and CloudWatch
# Logs instead; see the README.

resource "aws_lb" "this" {
  name               = "hive-fargate-test"
  internal           = true
  load_balancer_type = "application"
  security_groups    = [aws_security_group.alb.id]
  subnets            = aws_subnet.private[*].id
}

resource "aws_lb_target_group" "hive_service" {
  name        = "hive-fargate-test"
  port        = 8080
  protocol    = "HTTP"
  vpc_id      = aws_vpc.this.id
  target_type = "ip"

  health_check {
    path                = "/"
    matcher             = "200"
    interval            = 30
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }
}

resource "aws_lb_listener" "http" {
  load_balancer_arn = aws_lb.this.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.hive_service.arn
  }
}
