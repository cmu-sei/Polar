{ apiVersion = "v1"
, data.`gitlab.crt` = "INSERT_YOUR_BASE64_STRING"
, kind = "Secret"
, metadata =
  { creationTimestamp = None <>, name = "gitlab-crt", namespace = "polar" }
}
