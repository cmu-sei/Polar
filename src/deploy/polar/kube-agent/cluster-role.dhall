let kubernetes = ../../types/kubernetes.dhall  -- adjust path to your Kubernetes Dhall package
let values = ../values.dhall

let ClusterRole =
      kubernetes.ClusterRole::{
        apiVersion = "rbac.authorization.k8s.io/v1"
      , kind = "ClusterRole"
      , metadata = kubernetes.ObjectMeta::{
          , name = Some "kube-observer-read"
        }
      , rules = Some [
          kubernetes.PolicyRule::{
          , apiGroups = Some [""]
          , resources = Some [ "pods" ]
          , verbs = [ "get", "list", "watch" ]
          }
        ]
      }
in  ClusterRole
