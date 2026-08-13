{ apiVersion = "v1"
, data.`proxy.pem` = "INSERT_BASE64_STRING"
, kind = "Secret"
, metadata =
  { creationTimestamp = None <>, name = "proxy-ca-cert", namespace = "polar" }
}
