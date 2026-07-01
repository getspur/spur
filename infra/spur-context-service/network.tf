# Networking discovery.
#
# When the network vars are left empty (the original single-environment
# deployment, which runs on the account default VPC), these data sources
# resolve the VPC, its subnets, and its route tables automatically — so no
# VPC/subnet/route-table IDs need to be hard-coded in a tfvars file.
#
# staging/prod can still pin a dedicated non-default VPC by setting var.vpc_id
# (and optionally var.worker_subnets / var.worker_route_table_ids); when set,
# the explicit values win over discovery.

data "aws_vpc" "selected" {
  # Exactly one of default/id is set: empty vpc_id -> default VPC; otherwise by id.
  default = var.vpc_id == "" ? true : null
  id      = var.vpc_id != "" ? var.vpc_id : null
}

data "aws_subnets" "worker" {
  filter {
    name   = "vpc-id"
    values = [data.aws_vpc.selected.id]
  }
}

data "aws_route_tables" "worker" {
  vpc_id = data.aws_vpc.selected.id
}

locals {
  net_vpc_id          = data.aws_vpc.selected.id
  net_subnet_ids      = length(var.worker_subnets) > 0 ? var.worker_subnets : data.aws_subnets.worker.ids
  net_route_table_ids = length(var.worker_route_table_ids) > 0 ? var.worker_route_table_ids : data.aws_route_tables.worker.ids
}
