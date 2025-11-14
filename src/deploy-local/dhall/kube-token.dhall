{ apiVersion = "v1"
, kind = "Secret"
, metadata =
  { annotations.`kubernetes.io/service-account.name` = "kube-observer-sa"
  , name = "kube-observer-sa-token"
  , namespace = "polar"
  }
, type = "kubernetes.io/service-account-token"
}
