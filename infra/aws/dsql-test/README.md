# Aurora DSQL verification cluster

This configuration creates one Aurora DSQL cluster in the `hive-test` Amazon Web Services (AWS) account (`061705364908`, `eu-west-1`). It backs the verification work that reading AWS documentation cannot settle: which `CHECK` constraint, partial index, and composite `UNIQUE` constraint patterns Aurora DSQL accepts; how token-based Identity and Access Management (IAM) database authentication behaves, including token refresh; and how the optimistic concurrency claim-and-retry pattern behaves in place of `SELECT ... FOR UPDATE` and `SKIP LOCKED`, which Aurora DSQL does not support. `infra/aws/fargate-app` deploys against this same cluster.

## Prerequisite: the state-backend bootstrap must be applied first

This configuration's `backend "s3"` block (`versions.tf`) points at the Simple Storage Service (S3) bucket and DynamoDB lock table that `infra/aws/bootstrap/` creates. `terraform init` here fails on the backend until those exist, so apply the bootstrap first (see `infra/aws/bootstrap/README.md`).

## Resource schema

`terraform validate` (Terraform 1.15.8, `hashicorp/aws` 6.62.0) confirms the `aws_dsql_cluster` resource accepts `deletion_protection_enabled` (bool) and `tags` (map) with no other required arguments — AWS assigns the cluster identifier and endpoint; there is no user-supplied cluster name. `deletion_protection_enabled = false` is deliberate here: this cluster exists only for verification and must stay trivial to tear down, so it carries no production-style safeguard.

## Apply

```
export AWS_PROFILE=hive-test
cd infra/aws/dsql-test
terraform init
terraform plan
```

Review the plan before running `terraform apply` — this creates a real, billed AWS resource.
