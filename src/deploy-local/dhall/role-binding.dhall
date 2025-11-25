{ apiVersion = "rbac.authorization.k8s.io/v1"
, kind = "ClusterRole"
, metadata.name = "kube-observer-read"
, rules =
  [ { apiGroups = [ "" ]
    , resources =
      [ "namespaces", "pods", "services", "nodes", "deployments", "configmaps" ]
    , verbs = [ "get", "list", "watch" ]
    }
  ]
}
