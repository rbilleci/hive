# Fargate validation deployment

This configuration runs the `hive-service` container image (the Rust `hive` binary and the console
build, from the repository-root `Dockerfile`) on AWS Fargate in the
`hive-test` account (`061705364908`, `eu-west-1`), to validate that a real Fargate deployment
works end to end against the Aurora DSQL cluster `infra/aws/dsql-test` already provisions. It is a
minimum, non-production shape, but fully private: two private subnets with no internet gateway and
no NAT gateway anywhere in the VPC. The Fargate task has no public IP; it, and the internal ALB in
front of it, reach ECR, CloudWatch Logs, and Aurora DSQL only through the VPC interface endpoints
in `endpoints.tf` (plus a free S3 gateway endpoint for ECR image layers). It is not the production
ingress and deployment architecture `docs/architecture-specification-v1.1.md` defines — see that
document's management-plane ingress section for what a production build still needs (HTTPS, Okta
OIDC).

## Prerequisites

1. The state-backend bootstrap (`infra/aws/bootstrap/`) and the Aurora DSQL verification cluster
   (`infra/aws/dsql-test/`) must already be applied — this configuration reads the DSQL cluster's
   ARN, identifier, and PrivateLink connection service name from `dsql-test`'s remote state.
2. Docker, with a working daemon, to build the application image.
3. The `hive-test` AWS CLI profile authenticated (see `~/.aws/config`).

## Apply

```
export AWS_PROFILE=hive-test
cd infra/aws/fargate-app
terraform init
terraform plan
terraform apply
```

The first apply creates the ECR repository but the ECS service has no image to pull yet, so its
task will fail to start until the image below is pushed — expected on a first apply, not a fault.

## Blocked: Aurora DSQL authentication

**This stack cannot start the current image.** `ecs.tf` passes a `jdbc:aws-dsql:postgresql://` URL and
no password, because the Java service this stack was written for obtained IAM authentication tokens
through the AWS DSQL JDBC connector. The Rust connection factory accepts only a static user and
password. Until it generates and refreshes DSQL auth tokens (see the repository `README.md`), the task
exits at its first database connection. Nothing else in this stack is known to need a change: the
image listens on port 8080, reads the same `HIVE_*` variables, and answers the ALB probe on `/`.

## Build and push the application image

From the repository root (the multi-stage `Dockerfile` builds the console and the binary itself):

```
docker buildx build --platform linux/arm64 -t hive-service:latest .

aws ecr get-login-password --region eu-west-1 --profile hive-test \
  | docker login --username AWS --password-stdin "$(cd infra/aws/fargate-app && terraform output -raw ecr_repository_url | cut -d/ -f1)"

docker tag hive-service:latest "$(cd infra/aws/fargate-app && terraform output -raw ecr_repository_url):latest"
docker push "$(cd infra/aws/fargate-app && terraform output -raw ecr_repository_url):latest"

aws ecs update-service --cluster hive-test --service hive-service --force-new-deployment \
  --region eu-west-1 --profile hive-test
```

The push itself goes over the `ecr.api`/`ecr.dkr`/`s3` VPC endpoints only if you're running these
commands from inside the VPC; from a developer machine, the push uses your own normal internet
path to ECR's public API — only the *task's* pull at deploy time goes through the endpoints.

## Validate

The ALB is internal (`internal = true`) — there is no public subnet for it to use, so it is not
reachable from a laptop's browser or `curl`. Validate through the AWS control plane instead:

```
aws ecs wait services-stable --cluster hive-test --services hive-service --region eu-west-1 --profile hive-test

aws elbv2 describe-target-health --region eu-west-1 --profile hive-test \
  --target-group-arn "$(cd infra/aws/fargate-app && terraform output -raw target_group_arn)"

aws logs tail /ecs/hive-service --region eu-west-1 --profile hive-test --since 10m
```

A target health `State` of `healthy` confirms the ALB's own HTTP probe against `/` got a `200` from
inside the VPC. The log tail should show `migrate` completing and `hive-api: listening`, with no migration failure —
confirming the migrator ran successfully against the real cluster over the DSQL PrivateLink endpoint. Reaching the ALB from a browser would additionally need a bastion host,
a VPN/Direct Connect path, or a temporary SSM-endpoint-backed ECS Exec session — none of which this
configuration provisions, since the validation goal doesn't need them.

## Resources created

- `aws_vpc`/`aws_subnet` (2 private, no route to any internet or NAT gateway)/`aws_route_table`.
- `aws_vpc_endpoint.ecr_api`/`ecr_dkr`/`logs`/`dsql` (Interface, one AZ) and `aws_vpc_endpoint.s3`
  (Gateway, free): the only paths out of the private subnets.
- `aws_security_group.alb` (VPC-only ingress on 80)/`service` (ALB-only ingress on 8080)/`endpoints`
  (service-only ingress on 443 and 5432).
- `aws_ecr_repository.hive_service`: application image, with a 3-day untagged-image expiry.
- `aws_ecs_cluster.this` (`hive-test`), `aws_ecs_task_definition.hive_service` (ARM64, 1 vCPU/2GB),
  `aws_ecs_service.hive_service` (1 task, `assign_public_ip = false`).
- `aws_iam_role.task_execution`/`aws_iam_role.task`: separate execution role (image pull, log
  write, read the signing-key parameter) and task role (`dsql:DbConnectAdmin` on the DSQL cluster
  ARN only).
- `aws_ssm_parameter.identity_signing_key`: a `random_id`-generated `SecureString`, injected as the
  `HIVE_IDENTITY_SIGNING_KEY` container secret rather than a plain environment variable.
- `aws_lb` (internal)/`aws_lb_target_group`/`aws_lb_listener`: HTTP only, health check on `/`.
- `aws_cloudwatch_log_group./ecs/hive-service`: 7-day retention.

## Known simplifications, not to carry into a production build

- No HTTPS/ACM certificate, no Okta OIDC — anyone reaching the ALB from inside the VPC gets the
  app unauthenticated.
- Interface endpoints sit in a single AZ to minimize cost; a production build should place one ENI
  per AZ used by the task for resilience to an AZ-scoped endpoint failure.
- The application image is built on a developer machine from the multi-stage `Dockerfile`, not in a
  CI pipeline that pins base-image digests.
- Reuses `infra/aws/dsql-test`'s verification cluster rather than provisioning a dedicated,
  `deletion_protection_enabled = true` cluster for this deployment.
