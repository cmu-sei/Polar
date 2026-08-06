-- infra/layers/3-workloads/agents/provenance/resolver.dhall

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name             : Text
          , image            : Text
          , imagePullPolicy  : Text
          , imagePullSecrets : List { name : Optional Text }
          , polarInitImage   : Text
          , certIssuerUrl    : Text
          , saTokenAudience  : Text
          , proxyCACert      : Optional Text
          }
      ) ->
        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , kubernetes.Volume::{ name = Constants.OciRegistrySecret.name, secret = Some kubernetes.SecretVolumeSource::{ secretName = Some Constants.OciRegistrySecret.name, items = Some [ kubernetes.KeyToPath::{ key = "oci-registry-auth", path = "config.json" } ] } }
              , kubernetes.Volume::{ name = "polar-health", emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              ] # functions.ProxyVolume v.proxyCACert

        let env =
              Constants.commonClientEnv
              # functions.ProxyEnv v.proxyCACert
              # [ kubernetes.EnvVar::{ name = "POLAR_HEALTH_FILE",          value = Some "/var/run/polar-health/polar-health.json" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_CERTS",         value = Some "/etc/tls/certs/cert.pem" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_EXPIRY_SECS",   value = Some "60" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS",     value = Some "30" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_DEP_ENDPOINTS", value = Some "${Constants.cassiniDNSName}:${Natural/show Constants.cassiniPort}:300" }
                ]

        let mounts =
              [ Constants.certVolumeMount
              , kubernetes.VolumeMount::{ name = Constants.OciRegistrySecret.name, mountPath = "/home/polar/.docker/" }
              , kubernetes.VolumeMount::{ name = "polar-health", mountPath = "/var/run/polar-health" }
              ] # functions.ProxyMount v.proxyCACert

        let podSpec = kubernetes.PodSpec::{
              , imagePullSecrets = Some v.imagePullSecrets
              , restartPolicy    = Some "Never"
              , volumes          = Some volumes
              , initContainers   = Some
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
            , metadata = kubernetes.ObjectMeta::{ name = Some Constants.RegistryResolverName, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.JobSpec::{
              , backoffLimit            = Some 1000000
              , parallelism             = Some 1
              , completions             = Some 1
              , ttlSecondsAfterFinished = Some 300
              , template                = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{ name = Some Constants.RegistryResolverName, labels = Some [ { mapKey = "name", mapValue = Constants.RegistryResolverName } ] }
                , spec     = Some podSpec
                }
              }
            }

in render
