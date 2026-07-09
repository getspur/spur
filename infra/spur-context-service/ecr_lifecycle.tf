resource "aws_ecr_lifecycle_policy" "context_service_images" {
  for_each = var.manage_ecr_lifecycle_policies ? var.ecr_lifecycle_repository_names : toset([])

  repository = each.value

  policy = jsonencode({
    rules = [
      {
        rulePriority = 1
        description  = "Expire untagged images older than ${var.ecr_lifecycle_untagged_image_days} days"
        selection = {
          tagStatus   = "untagged"
          countType   = "sinceImagePushed"
          countUnit   = "days"
          countNumber = var.ecr_lifecycle_untagged_image_days
        }
        action = {
          type = "expire"
        }
      },
      {
        rulePriority = 2
        description  = "Keep only the most recent ${var.ecr_lifecycle_keep_tagged_images} tagged images"
        selection = {
          tagStatus      = "tagged"
          tagPatternList = ["*"]
          countType      = "imageCountMoreThan"
          countNumber    = var.ecr_lifecycle_keep_tagged_images
        }
        action = {
          type = "expire"
        }
      },
    ]
  })
}
