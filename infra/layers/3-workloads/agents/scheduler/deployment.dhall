-- infra/layers/3-workloads/agents/scheduler/deployment.dhall
--
-- Polar scheduler Deployment: observer + processor.
-- Observer syncs a GitOps schedules repo, processor writes graph nodes.
-- Git credentials for the schedules repo are mounted from a Secret.
-- No RBAC or SA token needed — pure messaging/graph workload.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name            : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , observer :
            { name         : Text
            , image        : Text
            , syncInterval : Text
            , localPath    : Text
            }
          , processor : { name : Text, image : Text }
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
          , proxyCACert    : Optional Text
          , neo4jBoltAddr  : Text
          , remoteUrl      : Text
          }
      ) ->

        let tlsVolume =
              kubernetes.Volume::{
              , name   = v.tls.certificateSpec.secretName
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some v.tls.certificateSpec.secretName
                }
              }

        let neo4jCAVolume =
              kubernetes.Volume::{
              , name   = "neo4j-bolt-ca"
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some "neo4j-bolt-ca"
                }
              }

        -- Git credentials for the schedules repo (username + password/token)
        let schedulerGitSecret =
              kubernetes.Volume::{
              , name   = "scheduler-git-secret"
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some "scheduler-git-secret"
                }
              }

        let volumes =
              [ tlsVolume, neo4jCAVolume, schedulerGitSecret ]
              # functions.ProxyVolume v.proxyCACert

        let tlsMount =
              kubernetes.VolumeMount::{
              , name      = v.tls.certificateSpec.secretName
              , mountPath = Constants.tlsPath
              , readOnly  = Some True
              }

        let neo4jCAMount =
              kubernetes.VolumeMount::{
              , name      = "neo4j-bolt-ca"
              , mountPath = "/etc/neo4j-ca"
              , readOnly  = Some True
              }

        let baseMounts = [ tlsMount ] # functions.ProxyMount v.proxyCACert

        let observerEnv =
              Constants.commonClientEnv
              # functions.ProxyEnv v.proxyCACert
              # [ kubernetes.EnvVar::{ name = "POLAR_SCHEDULER_REMOTE_URL",    value = Some v.remoteUrl           }
                , kubernetes.EnvVar::{ name = "POLAR_SCHEDULER_LOCAL_PATH",    value = Some v.observer.localPath  }
                , kubernetes.EnvVar::{ name = "POLAR_SCHEDULER_SYNC_INTERVAL", value = Some v.observer.syncInterval }
                , kubernetes.EnvVar::{
                  , name      = "POLAR_SCHEDULER_GIT_USERNAME"
                  , valueFrom = Some kubernetes.EnvVarSource::{
                    , secretKeyRef = Some kubernetes.SecretKeySelector::{
                      , name = Some "scheduler-git-secret"
                      , key  = "username"
                      }
                    }
                  }
                , kubernetes.EnvVar::{
                  , name      = "POLAR_SCHEDULER_GIT_PASSWORD"
                  , valueFrom = Some kubernetes.EnvVarSource::{
                    , secretKeyRef = Some kubernetes.SecretKeySelector::{
                      , name = Some "scheduler-git-secret"
                      , key  = "password"
                      }
                    }
                  }
                ]

        let processorEnv =
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
                  , imagePullSecrets = Some v.imagePullSecrets
                  , volumes          = Some volumes
                  , containers =
                    [ kubernetes.Container::{
                      , name            = v.observer.name
                      , image           = Some v.observer.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some observerEnv
                      , volumeMounts    = Some baseMounts
                      }
                    , kubernetes.Container::{
                      , name            = v.processor.name
                      , image           = Some v.processor.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some processorEnv
                      , volumeMounts    = Some (baseMounts # [ neo4jCAMount ])
                      }
                    ]
                  }
                }
              }
            }

in render
