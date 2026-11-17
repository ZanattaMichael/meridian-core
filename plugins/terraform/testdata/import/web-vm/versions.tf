terraform {
  required_version = ">= 1.6"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
    random = {
      source = "hashicorp/random"
    }
  }

  backend "s3" {
    bucket = "example-tfstate"
    key    = "web/terraform.tfstate"
    region = "us-east-1"
  }
}

provider "aws" {
  region = var.region

  default_tags {
    tags = local.tags
  }
}

provider "aws" {
  alias  = "dns"
  region = "us-east-1"
}
