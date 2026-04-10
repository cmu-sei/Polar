-- infra/layers/3-workloads/agents/kube/rbac.dhall
--
-- RBAC resources for the kube observer:
--   ClusterRole (read-only), ServiceAccount, ClusterRoleBinding, SA token Secret.
--
-- The SA token Secret uses the kubernetes.io/service-account-token type.
-- On a fresh cluster, kubectl apply may need to run twice — the token
-- controller populates the secret only after the ServiceAccount exists.
-- This is a known ordering issue tracked in INFRA.md.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall

let render =
      \(v :
          { observer :
            { serviceAccountName : Text
            , secretName         : Text
            }
          }
      ) ->

        let clusterRole =
              kubernetes.ClusterRole::{
              , apiVersion = "rbac.authorization.k8s.io/v1"
              , kind       = "ClusterRole"
              , metadata   = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }
              , rules      = Some
                [ kubernetes.PolicyRule::{
                  , apiGroups = Some [ "*" ]
                  , resources = Some [ "*" ]
                  , verbs     = [ "get", "list", "watch" ]
                  }
                ]
              }

        let serviceAccount =
              kubernetes.ServiceAccount::{
              , apiVersion = "v1"
              , kind       = "ServiceAccount"
              , metadata   = kubernetes.ObjectMeta::{
                , name      = Some v.observer.serviceAccountName
                , namespace = Some Constants.PolarNamespace
                }
              , automountServiceAccountToken = Some False
              }

        let roleBinding =
              kubernetes.RoleBinding::{
              , apiVersion = "rbac.authorization.k8s.io/v1"
              , kind       = "ClusterRoleBinding"
              , metadata   = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }
              , roleRef    = kubernetes.RoleRef::{
                , apiGroup = ""
                , kind     = "ClusterRole"
                , name     = "kube-observer-read"
                }
              , subjects = Some
                [ kubernetes.Subject::{
                  , kind      = "ServiceAccount"
                  , name      = v.observer.serviceAccountName
                  , namespace = Some Constants.PolarNamespace
                  }
                ]
              }

        let saToken =
              kubernetes.Secret::{
              , apiVersion = "v1"
              , kind       = "Secret"
              , metadata   = kubernetes.ObjectMeta::{
                , name        = Some v.observer.secretName
                , namespace   = Some Constants.PolarNamespace
                , annotations = Some
                  [ { mapKey   = "kubernetes.io/service-account.name"
                    , mapValue = v.observer.serviceAccountName
                    }
                  ]
                }
              , type = Some "kubernetes.io/service-account-token"
              }

        in  [ kubernetes.Resource.ClusterRole    clusterRole
            , kubernetes.Resource.ServiceAccount serviceAccount
            , kubernetes.Resource.RoleBinding    roleBinding
            , kubernetes.Resource.Secret         saToken
            ]

in render
