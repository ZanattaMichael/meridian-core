output "address" {
  description = "The public address of the web server."
  value       = aws_eip.web.public_ip
}

output "url" {
  value = "http://${aws_eip.web.public_dns}/"
}

output "instance_ids" {
  value     = aws_instance.web[*].id
  sensitive = true
}
