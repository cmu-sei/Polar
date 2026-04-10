-- infra/layers/3-workloads/agents/build/rbac.dhall
--
-- RBAC resources for the build orchestrator:
--   ClusterRole (read-only), ServiceAccount, ClusterRoleBinding, SA token Secret.
--
-- NOTE: SA token ordering issue applies here too — apply twice on fresh cluster.
-- See INFRA.md for details.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall

let clusterRoleName = "build-orchestrator-read"

let render =
      \(v :
          { orchestrator :
            { serviceAccountName : Text
            , secretName         : Text
            }
          }
      ) ->

        let clusterRole =
              kubernetes.ClusterRole::{
              , apiVersion = "rbac.authorization.k8s.io/v1"
              , kind       = "ClusterRole"
              , metadata   = kubernetes.ObjectMeta::{ name = Some clusterRoleName }
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
                , name      = Some v.orchestrator.serviceAccountName
                , namespace = Some Constants.PolarNamespace
                }
              , automountServiceAccountToken = Some False
              }

        let roleBinding =
              kubernetes.RoleBinding::{
              , apiVersion = "rbac.authorization.k8s.io/v1"
              , kind       = "ClusterRoleBinding"
              , metadata   = kubernetes.ObjectMeta::{ name = Some clusterRoleName }
              , roleRef    = kubernetes.RoleRef::{
                , apiGroup = ""
                , kind     = "ClusterRole"
                , name     = clusterRoleName
                }
              , subjects = Some
                [ kubernetes.Subject::{
                  , kind      = "ServiceAccount"
                  , name      = v.orchestrator.serviceAccountName
                  , namespace = Some Constants.PolarNamespace
                  }
                ]
              }

        let saToken =
              kubernetes.Secret::{
              , apiVersion = "v1"
              , kind       = "Secret"
              , metadata   = kubernetes.ObjectMeta::{
                , name        = Some v.orchestrator.secretName
                , namespace   = Some Constants.PolarNamespace
                , annotations = Some
                  [ { mapKey   = "kubernetes.io/service-account.name"
                    , mapValue = v.orchestrator.serviceAccountName
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
