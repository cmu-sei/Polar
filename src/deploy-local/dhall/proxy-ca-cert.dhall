{ apiVersion = "v1"
, data.`proxy.pem`
  =
    "INSERT_BASE64_CONTENT"
, kind = "Secret"
, metadata =
  { creationTimestamp = None <>, name = "proxy-ca-cert", namespace = "polar" }
}
