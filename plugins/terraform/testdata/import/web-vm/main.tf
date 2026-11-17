# The newest Ubuntu image.
data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"]

  filter {
    name   = "name"
    values = ["ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*"]
  }
}

resource "random_id" "suffix" {
  byte_length = 4
}

resource "aws_security_group" "web" {
  name = "${local.name}-${random_id.suffix.hex}"

  dynamic "ingress" {
    for_each = var.ports
    content {
      from_port   = ingress.value
      to_port     = ingress.value
      protocol    = "tcp"
      cidr_blocks = ["0.0.0.0/0"]
    }
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_instance" "web" {
  count                  = var.instance_count
  ami                    = data.aws_ami.ubuntu.id
  instance_type          = var.region == "us-east-1" ? "t3.small" : "t3.micro"
  vpc_security_group_ids = [aws_security_group.web.id]
  user_data              = <<-EOT
    #!/bin/sh
    echo "hello from ${local.name}" > /etc/motd
  EOT

  tags = merge(local.tags, {
    Index = count.index
  })

  lifecycle {
    create_before_destroy = true
    ignore_changes        = [ami]
    replace_triggered_by  = [aws_security_group.web]
  }
}

resource "aws_eip" "web" {
  instance = aws_instance.web[0].id
}

module "dns" {
  source  = "terraform-aws-modules/route53/aws//modules/records"
  version = "~> 2.0"

  providers = {
    aws = aws.dns
  }

  zone_name = "example.com"
  records = [
    {
      name    = "www"
      type    = "A"
      ttl     = 300
      records = [aws_eip.web.public_ip]
    },
  ]

  depends_on = [aws_instance.web]
}

moved {
  from = aws_instance.server
  to   = aws_instance.web
}
