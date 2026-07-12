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

# Cognito custom domains are backed by CloudFront and only accept ACM
# certificates issued in us-east-1, regardless of the user-pool region.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"
}
