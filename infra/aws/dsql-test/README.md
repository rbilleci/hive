# Aurora DSQL verification cluster

This configuration creates one Aurora DSQL cluster in the `hive-test` AWS account (`061705364908`, `eu-west-1`), for Phase 0 of the Aurora DSQL rewrite plan (`/Users/richard.billeci@mambu.com/.claude/plans/encapsulated-painting-pike.md`). It backs the verification work that can't be done by reading Amazon Web Services (AWS) documentation alone: confirming which `CHECK` constraint, partial index, and composite `UNIQUE` constraint patterns Aurora DSQL accepts, exercising IAM (Identity and Access Management) token-based JDBC (Java Database Connectivity) authentication and its refresh behavior, and prototyping the optimistic-concurrency-control (OCC) claim/retry pattern that replaces this codebase's `SELECT ... FOR UPDATE`/`SKIP LOCKED` usage. It later becomes the target for the hybrid test strategy's DSQL-specific tier once the rewrite reaches that phase.

## Prerequisite: the state-backend bootstrap must be applied first

This configuration's `backend "s3"` block (`versions.tf`) points at the S3 (Simple Storage Service) bucket and DynamoDB lock table that `infra/aws/bootstrap/` creates. That bootstrap configuration has been planned but not yet applied as of this writing — `terraform init` here will fail on the backend until it has been. Apply the bootstrap first (see `infra/aws/bootstrap/README.md`), then run `terraform init` here.

## Resource schema

`terraform validate` (Terraform 1.15.8, `hashicorp/aws` 6.62.0) confirms the `aws_dsql_cluster` resource accepts `deletion_protection_enabled` (bool) and `tags` (map) with no other required arguments — AWS assigns the cluster identifier and endpoint; there is no user-supplied cluster name. `deletion_protection_enabled = false` is deliberate here: this cluster exists only for Phase 0 verification and should be trivial to tear down once that work is done, not a candidate for a lasting production-style safeguard.

## Apply

```
export AWS_PROFILE=hive-test
cd infra/aws/dsql-test
terraform init
terraform plan
```

Review the plan before running `terraform apply` — this creates a real, billed AWS resource.
