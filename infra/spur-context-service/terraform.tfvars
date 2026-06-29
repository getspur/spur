aws_region = "ap-southeast-5"
vpc_id     = "vpc-09cf36dda830b0acf"
worker_subnets = [
  "subnet-0786603110b1af180",
  "subnet-03e3cdf70f69ab456",
  "subnet-0e57004af78597f73",
]
worker_route_table_ids = [
  "rtb-02f01b96fd72931d8",
]
worker_ecr_image    = "065285885105.dkr.ecr.ap-southeast-5.amazonaws.com/spur-context-worker:latest"
worker_lambda_image = "065285885105.dkr.ecr.ap-southeast-5.amazonaws.com/spur-context-worker-lambda:latest"
