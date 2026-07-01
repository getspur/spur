terraform {
  required_version = ">= 1.5"

  # Partial config; real values supplied via -backend-config=backends/<env>.s3.tfbackend
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
}
