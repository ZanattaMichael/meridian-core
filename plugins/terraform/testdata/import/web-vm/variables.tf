variable "region" {
  type        = string
  description = "Where the web server runs."
  default     = "eu-west-1"
}

variable "instance_count" {
  type    = number
  default = 2

  validation {
    condition     = var.instance_count > 0
    error_message = "At least one instance is needed."
  }
}

variable "ports" {
  type    = list(number)
  default = [80, 443]
}

locals {
  name = "web-${var.region}"
  tags = {
    Name    = local.name
    Managed = "terraform"
  }
}
