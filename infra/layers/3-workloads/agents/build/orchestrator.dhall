-- infra/layers/3-workloads/agents/build/orchestrator.dhall

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name               : Text
          , image              : Text
          , imagePullPolicy    : Text
          , imagePullSecrets   : List { name : Optional Text }
          , polarInitImage     : Text
          , certIssuerUrl      : Text
          , saTokenAudience    : Text
          , serviceAccountName : Text
          , secretName         : Text
          , proxyCACert        : Optional Text
          }
      ) ->
        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , kubernetes.Volume::{ name = v.secretName, secret = Some kubernetes.SecretVolumeSource::{ secretName = Some v.secretName } }
              , kubernetes.Volume::{ name = "build-orchestrator-config", secret = Some kubernetes.SecretVolumeSource::{ secretName = Some "build-orchestrator-config" } }
              , kubernetes.Volume::{ name = "polar-health", emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              ] # functions.ProxyVolume v.proxyCACert
        let env =
              Constants.commonClientEnv
              # [ kubernetes.EnvVar::{ name = "KUBE_TOKEN", valueFrom = Some kubernetes.EnvVarSource::{ secretKeyRef = Some kubernetes.SecretKeySelector::{ name = Some v.secretName, key = "token" } } }
                , kubernetes.EnvVar::{ name = "ORCHESTRATOR_CONFIG_FILE", value = Some "/etc/cyclops/cyclops.yaml" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_FILE",          value = Some "/var/run/polar-health/polar-health.json" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_CERTS",         value = Some "/etc/tls/certs/cert.pem" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_EXPIRY_SECS",   value = Some "60" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS",     value = Some "30" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_DEP_ENDPOINTS", value = Some "${Constants.cassiniDNSName}:${Natural/show Constants.cassiniPort}:300" }
                ]
        let mounts =
              [ Constants.certVolumeMount
              , kubernetes.VolumeMount::{ name = v.secretName, mountPath = "/var/run/secrets/kubernetes.io/serviceaccount" }
              , kubernetes.VolumeMount::{ name = "build-orchestrator-config", mountPath = "/etc/cyclops", readOnly = Some True }
              , kubernetes.VolumeMount::{ name = "polar-health", mountPath = "/var/run/polar-health" }
              ] # functions.ProxyMount v.proxyCACert

        let podSpec = kubernetes.PodSpec::{
              , imagePullSecrets   = Some v.imagePullSecrets
              , serviceAccountName = Some v.serviceAccountName
              , restartPolicy      = Some "Never"
              , volumes            = Some volumes
              , initContainers     = Some
                [ functions.makePolarInitContainer
                    v.polarInitImage
                    v.imagePullPolicy
                    Constants.saTokenVolumeName
                    v.certIssuerUrl
                    Constants.saTokenPath
                    [ kubernetes.VolumeMount::{ name = Constants.certVolumeName, mountPath = Constants.tlsPath } ]
                    ([] : List Text)
                    [ "client:${Constants.tlsPath}:ecdsa-p256:" ]
                ]
              , containers =
                [ kubernetes.Container::{
                  , name            = v.name
                  , image           = Some v.image
                  , imagePullPolicy = Some v.imagePullPolicy
                  , securityContext = Some Constants.DropAllCapSecurityContext
                  , env             = Some env
                  , volumeMounts    = Some mounts
                  , livenessProbe   = Some kubernetes.Probe::{
                      , exec                = Some { command = Some [ "polar-healthcheck" ] }
                      , initialDelaySeconds = Some 150
                      , periodSeconds       = Some 30
                      , failureThreshold    = Some 2
                      , timeoutSeconds      = Some 5
                      }
                  }
                ]
              }

        in  kubernetes.Job::{
            , metadata = kubernetes.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.JobSpec::{
              , backoffLimit            = Some 1000000
              , parallelism             = Some 1
              , completions             = Some 1
              , ttlSecondsAfterFinished = Some 300
              , template                = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                , spec     = Some podSpec
                }
              }
            }

in render
