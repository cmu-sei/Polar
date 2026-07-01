-- infra/layers/2-services/cassini/deployment.dhall
--
-- Cassini Job and Service.
-- Pure function: receives a values record, produces resources.

let k8s       = ../../../schema/kubernetes.dhall
let Constants = ../../../schema/constants.dhall
let functions = ../../../schema/functions.dhall

let render =
      \(v :
          { name             : Text
          , image            : Text
          , imagePullPolicy  : Text
          , imagePullSecrets : List { name : Optional Text }
          , polarInitImage   : Text
          , certIssuerUrl    : Text
          , saTokenAudience  : Text
          , ports            : { http : Natural, tcp : Natural }
          , jaegerDNSName    : Text
          , shutdownToken    : Text
          }
      ) ->
        let environment =
              [ k8s.EnvVar::{ name = "TLS_CA_CERT",           value = Some Constants.mtls.caCertPath }
              , k8s.EnvVar::{ name = "TLS_SERVER_CERT_CHAIN", value = Some Constants.mtls.certPath   }
              , k8s.EnvVar::{ name = "TLS_SERVER_KEY",        value = Some Constants.mtls.keyPath    }
              , k8s.EnvVar::{ name = "CASSINI_BIND_ADDR",     value = Some "0.0.0.0:${Natural/show v.ports.tcp}" }
              , k8s.EnvVar::{ name = "JAEGER_OTLP_ENDPOINT",  value = Some v.jaegerDNSName           }
              , k8s.EnvVar::{ name = "CASSINI_SHUTDOWN_TOKEN", value = Some v.shutdownToken           }
              , k8s.EnvVar::{ name = "CASSINI_LOG", value = Some "info" }
              , k8s.EnvVar::{ name = "POLAR_HEALTH_FILE",     value = Some "/var/run/polar-health/polar-health.json" }
              , k8s.EnvVar::{ name = "POLAR_HEALTH_CERTS",    value = Some "${Constants.mtls.certPath},${Constants.mtls.caCertPath}" }
              , k8s.EnvVar::{ name = "POLAR_HEALTH_EXPIRY_SECS", value = Some "60" }
              , k8s.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS",   value = Some "30" }
              ]

        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , k8s.Volume::{ name = "polar-health", emptyDir = Some k8s.EmptyDirVolumeSource::{=} }
              ]

        let mounts =
              [ k8s.VolumeMount::{ name = Constants.certVolumeName, mountPath = Constants.tlsPath, readOnly = Some True }
              , k8s.VolumeMount::{ name = "polar-health", mountPath = "/var/run/polar-health" }
              ]

        let job =
              k8s.Job::{
              , metadata = k8s.ObjectMeta::{
                , name      = Some v.name
                , namespace = Some Constants.PolarNamespace
                , annotations = Some [ Constants.RejectSidecarAnnotation ]
                }
              , spec = Some k8s.JobSpec::{
                , backoffLimit            = Some 1000000
                , parallelism             = Some 1
                , completions             = Some 1
                , ttlSecondsAfterFinished = Some 300
                , template = k8s.PodTemplateSpec::{
                  , metadata = Some k8s.ObjectMeta::{
                    , name   = Some v.name
                    , labels = Some [ { mapKey = "name", mapValue = v.name } ]
                    }
                  , spec = Some k8s.PodSpec::{
                    , imagePullSecrets = Some v.imagePullSecrets
                    , restartPolicy   = Some "Never"
                    , volumes         = Some volumes
                    , initContainers  = Some
                      [ functions.makePolarInitContainer
                          v.polarInitImage
                          v.imagePullPolicy
                          Constants.saTokenVolumeName
                          v.certIssuerUrl
                          Constants.saTokenPath
                          [ k8s.VolumeMount::{ name = Constants.certVolumeName, mountPath = Constants.tlsPath } ]
                          ([] : List Text)
                          [ "server:${Constants.tlsPath}:ecdsa-p256:${Constants.cassiniDNSName}" ]
                      ]
                    , containers =
                      [ k8s.Container::{
                        , name            = "cassini"
                        , image           = Some v.image
                        , imagePullPolicy = Some v.imagePullPolicy
                        , securityContext = Some Constants.DropAllCapSecurityContext
                        , env             = Some environment
                        , ports           = Some
                          [ k8s.ContainerPort::{ containerPort = v.ports.tcp  }
                          , k8s.ContainerPort::{ containerPort = v.ports.http }
                          ]
                        , volumeMounts    = Some mounts
                        }
                      ]
                    }
                  }
                }
              }

        let service =
              k8s.Service::{
              , metadata = k8s.ObjectMeta::{ name = Some Constants.cassiniService.name, namespace = Some Constants.PolarNamespace }
              , spec = Some k8s.ServiceSpec::{
                , selector = Some (toMap { name = v.name })
                , type     = Some Constants.cassiniService.type
                , ports    = Some
                  [ k8s.ServicePort::{ name = Some "cassini-tcp",  targetPort = Some (k8s.NatOrString.Nat v.ports.tcp),  port = v.ports.tcp  }
                  , k8s.ServicePort::{ name = Some "cassini-http", targetPort = Some (k8s.NatOrString.Nat v.ports.http), port = v.ports.http }
                  ]
                }
              }

        in  [ k8s.Resource.Service service, k8s.Resource.Job job ]

in render
