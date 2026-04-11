-- infra/layers/3-workloads/agents/gitlab/observer.dhall
--
-- GitLab observer deployment.
-- Polls GitLab API and publishes events to Cassini topics.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name            : Text
          , image           : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , endpoint        : Text
          , baseIntervalSecs : Natural
          , tlsSecretName   : Text
          , proxyCACert     : Optional Text
          }
      ) ->

        let tlsMount =
              kubernetes.VolumeMount::{
              , name      = v.tlsSecretName
              , mountPath = Constants.tlsPath
              , readOnly  = Some True
              }

        let volumes =
              [ kubernetes.Volume::{
                , name   = v.tlsSecretName
                , secret = Some kubernetes.SecretVolumeSource::{ secretName = Some v.tlsSecretName }
                }
              ] # functions.ProxyVolume v.proxyCACert

        let env =
              Constants.commonClientEnv
              # functions.ProxyEnv v.proxyCACert
              # [ kubernetes.EnvVar::{ name = "OBSERVER_BASE_INTERVAL", value = Some (Natural/show v.baseIntervalSecs) }
                , kubernetes.EnvVar::{ name = "GITLAB_ENDPOINT",        value = Some v.endpoint }
                , kubernetes.EnvVar::{
                  , name      = "GITLAB_TOKEN"
                  , valueFrom = Some kubernetes.EnvVarSource::{
                    , secretKeyRef = Some kubernetes.SecretKeySelector::{
                      , name = Some "gitlab-secret"
                      , key  = "token"
                      }
                    }
                  }
                ]

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
                      , name            = v.name
                      , image           = Some v.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some env
                      , volumeMounts    = Some ([ tlsMount ] # functions.ProxyMount v.proxyCACert)
                      }
                    ]
                  }
                }
              }
            }

in render
