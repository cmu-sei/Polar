let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

let subject = kubernetes.Subject::{ kind = "ServiceAccount"
, name = "kube-observer-sa"
, namespace = Some values.namespace
}

in kubernetes.RoleBinding::{ 
    apiVersion = "rbac.authorization.k8s.io/v1"
, kind = "RoleBinding"
, metadata = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }
, roleRef = kubernetes.RoleRef::{ apiGroup = "", kind = "ClusterRole", name = "kube-observer-read" }
, subjects = Some [ subject ]

}
