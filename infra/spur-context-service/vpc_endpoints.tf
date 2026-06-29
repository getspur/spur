locals {
  vpc_endpoint_region = coalesce(var.vpc_endpoint_region, var.aws_region)

  gateway_vpc_endpoint_services = {
    s3       = "com.amazonaws.${local.vpc_endpoint_region}.s3"
    dynamodb = "com.amazonaws.${local.vpc_endpoint_region}.dynamodb"
  }

  interface_vpc_endpoint_services = {
    states         = "com.amazonaws.${local.vpc_endpoint_region}.states"
    secretsmanager = "com.amazonaws.${local.vpc_endpoint_region}.secretsmanager"
    ecr_api        = "com.amazonaws.${local.vpc_endpoint_region}.ecr.api"
    ecr_dkr        = "com.amazonaws.${local.vpc_endpoint_region}.ecr.dkr"
    logs           = "com.amazonaws.${local.vpc_endpoint_region}.logs"
    sts            = "com.amazonaws.${local.vpc_endpoint_region}.sts"
  }
}

resource "aws_vpc_endpoint" "gateway" {
  for_each = var.create_vpc_endpoints ? local.gateway_vpc_endpoint_services : {}

  vpc_id            = var.vpc_id
  service_name      = each.value
  vpc_endpoint_type = "Gateway"
  route_table_ids   = var.worker_route_table_ids

  tags = {
    Name = "spur-context-${each.key}-gateway-endpoint"
  }

  lifecycle {
    precondition {
      condition     = length(var.worker_route_table_ids) > 0
      error_message = "worker_route_table_ids must include the route table IDs associated with worker_subnets when create_vpc_endpoints is true."
    }
  }
}

resource "aws_vpc_endpoint" "interface" {
  for_each = var.create_vpc_endpoints ? local.interface_vpc_endpoint_services : {}

  vpc_id              = var.vpc_id
  service_name        = each.value
  vpc_endpoint_type   = "Interface"
  subnet_ids          = var.worker_subnets
  security_group_ids  = [aws_security_group.vpc_endpoints[0].id]
  private_dns_enabled = true

  tags = {
    Name = "spur-context-${replace(each.key, "_", "-")}-interface-endpoint"
  }
}
