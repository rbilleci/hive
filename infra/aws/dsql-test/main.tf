resource "aws_dsql_cluster" "verification" {
  deletion_protection_enabled = false

  tags = {
    Name    = "hive-dsql-verification"
    Purpose = "Phase-0 DSQL rewrite verification: CHECK-index-UNIQUE syntax + IAM auth + OCC prototype"
  }
}
