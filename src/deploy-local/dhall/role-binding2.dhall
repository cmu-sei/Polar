{ apiVersion = "rbac.authorization.k8s.io/v1"
, kind = "ClusterRoleBinding"
, metadata.name = "kube-observer-read-binding"
, roleRef =
  { apiGroup = "rbac.authorization.k8s.io"
  , kind = "ClusterRole"
  , name = "kube-observer-read"
  }
, subjects =
  [ { kind = "ServiceAccount", name = "kube-observer-sa", namespace = "polar" }
  ]
}
