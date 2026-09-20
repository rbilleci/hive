output "identifier" {
  value = aws_dsql_cluster.verification.identifier
}

output "arn" {
  value = aws_dsql_cluster.verification.arn
}

output "endpoint" {
  description = "Aurora DSQL cluster public endpoint hostname (no port; the connector always uses 5432)."
  value       = "${aws_dsql_cluster.verification.identifier}.dsql.eu-west-1.on.aws"
}

output "vpc_endpoint_service_name" {
  description = "Cluster-specific PrivateLink connection service name, e.g. com.amazonaws.eu-west-1.dsql-xxxx."
  value       = aws_dsql_cluster.verification.vpc_endpoint_service_name
}
