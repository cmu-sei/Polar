{ apiVersion = "v1"
, automountServiceAccountToken = False
, kind = "ServiceAccount"
, metadata = { name = "kube-observer-sa", namespace = "polar" }
}
