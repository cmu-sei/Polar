{ apiVersion = "v1"
, data.`gitlab-crt.crt`
  =
    "INSERT_BASE64_CONTENT"
, kind = "Secret"
, metadata =
  { creationTimestamp = None <>, name = "gitlab-crt", namespace = "polar" }
}
