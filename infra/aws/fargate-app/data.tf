data "aws_availability_zones" "available" {
  state = "available"
}

data "terraform_remote_state" "dsql_test" {
  backend = "s3"
  config = {
    bucket = "hive-test-tfstate-061705364908"
    key    = "hive/test/dsql-test/terraform.tfstate"
    region = "eu-west-1"
  }
}
