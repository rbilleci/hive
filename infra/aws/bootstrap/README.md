# Terraform state bootstrap

This configuration creates the Amazon Simple Storage Service (S3) bucket and Amazon DynamoDB table that hold Terraform's remote state for the `hive-test` AWS account (`061705364908`, `eu-west-1`). Every other Terraform configuration in `infra/aws/` reads and writes state through these two resources, so this configuration is the one exception that keeps its own state locally — no remote backend exists yet when it first runs.

## Apply

Authenticate with the `hive-test` AWS Command Line Interface (CLI) profile (see `~/.aws/config`), then run Terraform with that profile active:

```
export AWS_PROFILE=hive-test
cd infra/aws/bootstrap
terraform init
terraform apply
```

## Resources created

- `aws_s3_bucket.tfstate`: bucket `hive-test-tfstate-061705364908`, versioned, AES256-encrypted, all public access blocked, ACLs disabled via `BucketOwnerEnforced`.
- `aws_dynamodb_table.tfstate_lock`: table `hive-test-tfstate-lock`, on-demand (`PAY_PER_REQUEST`) billing, partition key `LockID`.

## Backend block for downstream configurations

Every other `infra/aws/` configuration for this account points its backend at these two resources:

```hcl
terraform {
  backend "s3" {
    bucket       = "hive-test-tfstate-061705364908"
    key          = "hive/test/<config-name>/terraform.tfstate"
    region       = "eu-west-1"
    use_lockfile = true
    encrypt      = true
  }
}
```

Replace `<config-name>` with the name of that configuration's directory under `infra/aws/`, so each configuration's state occupies a distinct key in the shared bucket.
