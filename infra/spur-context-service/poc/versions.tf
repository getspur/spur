terraform {
  required_version = ">= 1.9, < 2.0"

  # This backend is intentionally configured only through the POC-specific
  # partial backend file. Never pass a production backend file to this root.
  backend "s3" {}

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}

provider "aws" {
  region = var.aws_region

  default_tags {
    tags = local.common_tags
  }
}
