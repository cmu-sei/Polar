-- infra/layers/3-workloads/agents/scheduler/observer.dhall
--
-- Credentials (POLAR_SCHEDULER_GIT_USERNAME / POLAR_SCHEDULER_GIT_PASSWORD)
-- have been removed. The polar-schedules repo is public and does not require
-- authentication. Private repo credential support will come via the
-- CredentialAgent (see credential-agent issue).

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name            : Text
          , image           : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , syncInterval    : Text
          , localPath       : Text
          , remoteUrl       : Text
          , tlsSecretName   : Text
          , proxyCACert     : Optional Text
          }
      ) ->

        let volumes =
              [ kubernetes.Volume::{ name = v.tlsSecretName, secret = Some kubernetes.SecretVolumeSource::{ secretName = Some v.tlsSecretName } }
              ] # functions.ProxyVolume v.proxyCACert

        let env =
              Constants.commonClientEnv
              # functions.ProxyEnv v.proxyCACert
              # [ kubernetes.EnvVar::{ name = "POLAR_SCHEDULER_REMOTE_URL",    value = Some v.remoteUrl    }
                , kubernetes.EnvVar::{ name = "POLAR_SCHEDULER_LOCAL_PATH",    value = Some v.localPath    }
                , kubernetes.EnvVar::{ name = "POLAR_SCHEDULER_SYNC_INTERVAL", value = Some v.syncInterval }
                ]

        let mounts =
              [ kubernetes.VolumeMount::{ name = v.tlsSecretName, mountPath = Constants.tlsPath, readOnly = Some True }
              ] # functions.ProxyMount v.proxyCACert

        in  kubernetes.Deployment::{
            , metadata = kubernetes.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.DeploymentSpec::{
              , selector = kubernetes.LabelSelector::{ matchLabels = Some (toMap { name = v.name }) }
              , replicas = Some 1
              , template = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                , spec = Some kubernetes.PodSpec::{
                  , imagePullSecrets = Some v.imagePullSecrets
                  , volumes          = Some volumes
                  , containers =
                    [ kubernetes.Container::{
                      , name            = v.name
                      , image           = Some v.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some env
                      , volumeMounts    = Some mounts
                      }
                    ]
                  }
                }
              }
            }

in render
