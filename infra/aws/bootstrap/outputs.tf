output "state_bucket" {
  value = aws_s3_bucket.tfstate.id
}

output "lock_table" {
  value = aws_dynamodb_table.tfstate_lock.id
}

output "region" {
  value = "eu-west-1"
}
