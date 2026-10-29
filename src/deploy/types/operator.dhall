{-
  types/operator.dhall — the polar-operator itself: the identity it runs as, the access it needs,
  and the Deployment that keeps it alive.

  The access is exactly what the operator calls, and no more:

    PolarAgent resources     read them, write their status
    Pods                     read them, create the ones an agent runs as, delete the ones it ends
    ServiceAccounts          create the identity an agent runs as when none is named
    the kube-system namespace  read it: its UID is the cluster's identity in the graph

  The CustomResourceDefinition is not rendered here. It is generated from the operator's own types,
  so it cannot disagree with them:

    polar-operator crd | kubectl apply -f -

  Deploy it before any PolarAgent.

  Without BROKER_ADDR the operator keeps agents alive and publishes nothing. Giving it a broker means
  giving it the certificates to reach one (a polar-init init container, as an agent has); that is not
  done here, and `env` is where the broker's variables would go.
-}
let kubernetes = ./kubernetes.dhall

let Operator =
      { Type =
          { namespace : Text
          , image : Text
          , name : Text
          , env : List kubernetes.EnvVar.Type
          , runAsUser : Natural
          }
      , default =
        { name = "polar-operator"
        , env = [] : List kubernetes.EnvVar.Type
        , runAsUser = 65532
        }
      }

let rule =
      λ(apiGroups : List Text) →
      λ(resources : List Text) →
      λ(verbs : List Text) →
        kubernetes.PolicyRule::{
        , apiGroups = Some apiGroups
        , resources = Some resources
        , verbs
        }

let makeOperator
    : Operator.Type → List kubernetes.Resource
    = λ(operator : Operator.Type) →
        let labels = [ { mapKey = "app", mapValue = operator.name } ]

        let metadata =
              kubernetes.ObjectMeta::{
              , name = Some operator.name
              , namespace = Some operator.namespace
              }

        let clusterMetadata = kubernetes.ObjectMeta::{ name = Some operator.name }

        in  [ kubernetes.Resource.ServiceAccount
                kubernetes.ServiceAccount::{
                , metadata
                , automountServiceAccountToken = Some True
                }
            , kubernetes.Resource.ClusterRole
                kubernetes.ClusterRole::{
                , metadata = clusterMetadata
                , rules = Some
                  [ rule [ "polar.sei.cmu.edu" ] [ "polaragents" ] [ "get", "list", "watch" ]
                  , rule
                      [ "polar.sei.cmu.edu" ]
                      [ "polaragents/status" ]
                      [ "get", "patch", "update" ]
                  , rule
                      [ "" ]
                      [ "pods" ]
                      [ "get", "list", "watch", "create", "delete" ]
                  , rule [ "" ] [ "serviceaccounts" ] [ "get", "create" ]
                  , rule [ "" ] [ "namespaces" ] [ "get" ]
                  ]
                }
            , kubernetes.Resource.ClusterRoleBinding
                kubernetes.ClusterRoleBinding::{
                , metadata = clusterMetadata
                , roleRef =
                  { apiGroup = "rbac.authorization.k8s.io"
                  , kind = "ClusterRole"
                  , name = operator.name
                  }
                , subjects = Some
                  [ kubernetes.Subject::{
                    , kind = "ServiceAccount"
                    , name = operator.name
                    , namespace = Some operator.namespace
                    }
                  ]
                }
            , kubernetes.Resource.Deployment
                kubernetes.Deployment::{
                , metadata
                , spec = Some kubernetes.DeploymentSpec::{
                  , replicas = Some 1
                  -- One operator, never two: a second would race the first for every agent.
                  , strategy = Some kubernetes.DeploymentStrategy::{
                    , type = Some "Recreate"
                    }
                  , selector = kubernetes.LabelSelector::{
                    , matchLabels = Some labels
                    }
                  , template = kubernetes.PodTemplateSpec::{
                    , metadata = Some kubernetes.ObjectMeta::{
                      , name = Some operator.name
                      , labels = Some labels
                      }
                    , spec = Some kubernetes.PodSpec::{
                      , serviceAccountName = Some operator.name
                      , containers =
                        [ kubernetes.Container::{
                          , name = "operator"
                          , image = Some operator.image
                          -- The image names its binary as a default command, not an entrypoint, so
                          -- giving only `args` would replace it. Name both.
                          , command = Some [ "polar-operator" ]
                          , args = Some [ "run" ]
                          , env = Some operator.env
                          , securityContext = Some kubernetes.SecurityContext::{
                            , runAsNonRoot = Some True
                            , runAsUser = Some operator.runAsUser
                            , runAsGroup = Some operator.runAsUser
                            , readOnlyRootFilesystem = Some True
                            , allowPrivilegeEscalation = Some False
                            , capabilities = Some kubernetes.Capabilities::{
                              , drop = Some [ "ALL" ]
                              }
                            }
                          }
                        ]
                      }
                    }
                  }
                }
            ]

in  { Operator, makeOperator }
