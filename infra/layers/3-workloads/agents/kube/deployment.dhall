-- infra/layers/3-workloads/agents/kube/deployment.dhall
--
-- Kubernetes agent Deployment.
-- Observer watches cluster API via mounted SA token.
-- Consumer reads Cassini topics and writes to the graph.
-- Both containers reject Istio sidecar injection.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name            : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , observer :
            { name               : Text
            , image              : Text
            , serviceAccountName : Text
            , secretName         : Text
            }
          , consumer :
            { name  : Text
            , image : Text
            }
          , tls :
            { certificateRequestName : Text
            , certificateSpec :
              { commonName  : Text
              , dnsNames    : List Text
              , duration    : Text
              , issuerRef   : { kind : Text, name : Text }
              , renewBefore : Text
              , secretName  : Text
              }
            }
          , proxyCACert   : Optional Text
          , neo4jBoltAddr : Text
          }
      ) ->

        let tlsVolume =
              kubernetes.Volume::{
              , name   = v.tls.certificateSpec.secretName
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some v.tls.certificateSpec.secretName
                }
              }

        let saTokenVolume =
              kubernetes.Volume::{
              , name   = v.observer.secretName
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some v.observer.secretName
                }
              }

        let neo4jCAVolume =
              kubernetes.Volume::{
              , name   = "neo4j-bolt-ca"
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some "neo4j-bolt-ca"
                }
              }

        let volumes =
              [ tlsVolume, saTokenVolume, neo4jCAVolume ]
              # functions.ProxyVolume v.proxyCACert

        let tlsMount =
              kubernetes.VolumeMount::{
              , name      = v.tls.certificateSpec.secretName
              , mountPath = Constants.tlsPath
              , readOnly  = Some True
              }

        let saTokenMount =
              kubernetes.VolumeMount::{
              , name      = v.observer.secretName
              , mountPath = "/var/run/secrets/kubernetes.io/serviceaccount"
              }

        let neo4jCAMount =
              kubernetes.VolumeMount::{
              , name      = "neo4j-bolt-ca"
              , mountPath = "/etc/neo4j-ca"
              , readOnly  = Some True
              }

        let observerEnv =
              Constants.commonClientEnv
              # functions.ProxyEnv v.proxyCACert
              # [ kubernetes.EnvVar::{
                  , name      = "KUBE_TOKEN"
                  , valueFrom = Some kubernetes.EnvVarSource::{
                    , secretKeyRef = Some kubernetes.SecretKeySelector::{
                      , name = Some v.observer.secretName
                      , key  = "token"
                      }
                    }
                  }
                ]

        let consumerEnv =
              Constants.commonClientEnv
              # functions.makeGraphEnv
                  v.neo4jBoltAddr
                  Constants.graphConfig
                  Constants.graphSecretKeySelector
                  (Some "/etc/neo4j-ca/ca.pem")

        in  kubernetes.Deployment::{
            , metadata = kubernetes.ObjectMeta::{
              , name        = Some v.name
              , namespace   = Some Constants.PolarNamespace
              , annotations = Some [ Constants.RejectSidecarAnnotation ]
              }
            , spec = Some kubernetes.DeploymentSpec::{
              , selector = kubernetes.LabelSelector::{
                , matchLabels = Some (toMap { name = v.name })
                }
              , replicas = Some 1
              , template = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{
                  , name   = Some v.name
                  , labels = Some [ { mapKey = "name", mapValue = v.name } ]
                  }
                , spec = Some kubernetes.PodSpec::{
                  , imagePullSecrets   = Some v.imagePullSecrets
                  , serviceAccountName = Some v.observer.serviceAccountName
                  , volumes            = Some volumes
                  , containers =
                    [ kubernetes.Container::{
                      , name            = v.observer.name
                      , image           = Some v.observer.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some observerEnv
                      , volumeMounts    = Some [ tlsMount, saTokenMount ]
                      }
                    , kubernetes.Container::{
                      , name            = v.consumer.name
                      , image           = Some v.consumer.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some consumerEnv
                      , volumeMounts    = Some [ tlsMount, neo4jCAMount ]
                      }
                    ]
                  }
                }
              }
            }

in render
