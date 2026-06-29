terraform {
  required_version = ">= 1.5"

  # backend "s3" {}  # TEMP: neutralized for local-state in-place upgrade trial; restore before commit

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
