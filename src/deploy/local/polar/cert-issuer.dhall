{-
  local/cert-issuer.dhall — cert issuer deployment for local cluster development.

  Apply this before cassini.dhall and agents.dhall. Every agent's init
  container calls the cert issuer at pod startup — if it isn't running,
  every pod stays in Init:CrashLoopBackOff until it is.

  Deployment order:
    1. kubectl apply -f cert-issuer.dhall
    2. kubectl rollout status deploy/cert-issuer -n polar
    3. kubectl apply -f cassini.dhall
    4. kubectl apply -f agents.dhall
-}

let kubernetes = ../../types/kubernetes.dhall
let Constants  = ../../types/constants.dhall
let values     = ../values.dhall

let certIssuerServiceAccount =
      kubernetes.ServiceAccount::{
      , apiVersion = "v1"
      , kind       = "ServiceAccount"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some values.certIssuer.serviceAccountName
        , namespace = Some Constants.PolarNamespace
        }
      , automountServiceAccountToken = Some True
      }

let certIssuerClusterRole =
      kubernetes.ClusterRole::{
      , apiVersion = "rbac.authorization.k8s.io/v1"
      , kind       = "ClusterRole"
      , metadata   = kubernetes.ObjectMeta::{ name = Some "cert-issuer-token-review" }
      , rules      = Some
        [ kubernetes.PolicyRule::{
          , apiGroups = Some [ "authentication.k8s.io" ]
          , resources = Some [ "tokenreviews" ]
          , verbs     = [ "create" ]
          }
        ]
      }

let certIssuerClusterRoleBinding =
      kubernetes.RoleBinding::{
      , apiVersion = "rbac.authorization.k8s.io/v1"
      , kind       = "ClusterRoleBinding"
      , metadata   = kubernetes.ObjectMeta::{ name = Some "cert-issuer-token-review" }
      , roleRef    = kubernetes.RoleRef::{
        , apiGroup = "rbac.authorization.k8s.io"
        , kind     = "ClusterRole"
        , name     = "cert-issuer-token-review"
        }
      , subjects = Some
        [ kubernetes.Subject::{
          , kind      = "ServiceAccount"
          , name      = values.certIssuer.serviceAccountName
          , namespace = Some Constants.PolarNamespace
          }
        ]
      }

let certIssuerConfigMap =
      kubernetes.ConfigMap::{
      , apiVersion = "v1"
      , kind       = "ConfigMap"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some "cert-issuer-config"
        , namespace = Some Constants.PolarNamespace
        }
      , data = Some
        [ { mapKey = "config.json", mapValue = values.certIssuer.config } ]
      }

let certIssuerPvc =
      kubernetes.PersistentVolumeClaim::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some "cert-issuer-ca"
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some kubernetes.PersistentVolumeClaimSpec::{
        , accessModes = Some [ "ReadWriteOnce" ]
        , resources   = Some kubernetes.VolumeResourceRequirements::{
          , requests = Some [ { mapKey = "storage", mapValue = "1Gi" } ]
          }
        }
      }

let certIssuerService =
    kubernetes.Service::{
    , metadata = kubernetes.ObjectMeta::{
        , name      = Some "cert-issuer"
        , namespace = Some Constants.PolarNamespace
        }
    , spec = Some kubernetes.ServiceSpec::{
        , selector = Some (toMap { name = values.certIssuer.name })
        , type     = Some "ClusterIP"
        , ports    = Some
        [ kubernetes.ServicePort::{
            , name       = Some "cert-issuer-http"
            , port       = 8443
            , targetPort = Some (kubernetes.NatOrString.Nat 8443)
            }
        ]
        }
    }
let certIssuerDeployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some values.certIssuer.name
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , replicas = Some 1
        , strategy = Some kubernetes.DeploymentStrategy::{ type = Some "Recreate" }
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = values.certIssuer.name })
          }
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name   = Some values.certIssuer.name
            , labels = Some [ { mapKey = "name", mapValue = values.certIssuer.name } ]
            }
          , spec = Some kubernetes.PodSpec::{
            , serviceAccountName = Some values.certIssuer.serviceAccountName
            , imagePullSecrets   = Some values.imagePullSecrets
            , containers =
              [ kubernetes.Container::{
                , name            = values.certIssuer.name
                , image           = Some values.certIssuer.image
                , imagePullPolicy = Some values.imagePullPolicy
                , securityContext = Some Constants.DropAllCapSecurityContext
                , env = Some
                  [ kubernetes.EnvVar::{ name = "RUST_LOG",             value = Some "debug" }
                  , kubernetes.EnvVar::{ name = "CERT_ISSUER_CONFIG",   value = Some "/etc/cert-issuer/config.json" }
                  , kubernetes.EnvVar::{
                    , name  = "KUBERNETES_CA_CERT"
                    , value = Some "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
                    }
                    , kubernetes.EnvVar::{
                    , name  = "KUBERNETES_SA_TOKEN"
                    , value = Some "/var/run/secrets/kubernetes.io/serviceaccount/token"
                    }
                  ]
                , ports = Some
                  [ kubernetes.ContainerPort::{ containerPort = 8443 } ]
                , volumeMounts = Some
                  [ kubernetes.VolumeMount::{
                    , name      = "cert-issuer-config"
                    , mountPath = "/etc/cert-issuer/config.json"
                    , subPath   = Some "config.json"
                    , readOnly  = Some True
                    }
                  , kubernetes.VolumeMount::{
                    , name      = "cert-issuer-ca"
                    , mountPath = "/home/polar/ca"
                    }
                  ]
                }
              ]
            , volumes = Some
              [ kubernetes.Volume::{
                , name      = "cert-issuer-config"
                , configMap = Some kubernetes.ConfigMapVolumeSource::{
                  , name = Some "cert-issuer-config"
                  }
                }
              , kubernetes.Volume::{
                , name                  = "cert-issuer-ca"
                , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                  , claimName = "cert-issuer-ca"
                  }
                }
              ]
            }
          }
        }
      }

in  [ kubernetes.Resource.ClusterRole             certIssuerClusterRole
    , kubernetes.Resource.RoleBinding             certIssuerClusterRoleBinding
    , kubernetes.Resource.ServiceAccount          certIssuerServiceAccount
    , kubernetes.Resource.ConfigMap               certIssuerConfigMap
    , kubernetes.Resource.PersistentVolumeClaim   certIssuerPvc
    , kubernetes.Resource.Service                 certIssuerService
    , kubernetes.Resource.Deployment              certIssuerDeployment
    ]
