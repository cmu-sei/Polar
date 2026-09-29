-- infra/layers/2-services/credentials-agent/observer.dhall
--
-- Credentials Agent observer -- the real, direct-HTTP-facing gateway
-- (POST /issue, POST /identity) to the async Cassini request/reply
-- protocol every other endpoint in this system uses. Job (not
-- Deployment) for the same, established reason as every other
-- long-running agent's own manifest -- see deployment.dhall's own
-- top comment. Job plus a real Service, mirroring cassini's own
-- deployment.dhall shape exactly, since external callers (a human's
-- tool, an admin bootstrap tool) need to reach this directly over
-- HTTP, unlike the consumer half, which is a pure Cassini pub/sub
-- participant with no direct network endpoint of its own at all.

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
          , port             : Natural
          -- Name of the ConfigMap holding the Polar CA certificate, or None
          -- while cert-issuer self-bootstraps. See deployment.dhall.
          , caConfigMap      : Optional Text
          }
      ) ->
        let caVolumes =
              merge
                { Some = \(cm : Text) ->
                    [ k8s.Volume::{
                      , name      = "polar-ca"
                      , configMap = Some k8s.ConfigMapVolumeSource::{ name = Some cm }
                      }
                    ]
                , None = [] : List k8s.Volume.Type
                }
                v.caConfigMap
        let caMounts =
              merge
                { Some = \(_ : Text) ->
                    [ k8s.VolumeMount::{ name = "polar-ca", mountPath = "/etc/polar-ca", readOnly = Some True } ]
                , None = [] : List k8s.VolumeMount.Type
                }
                v.caConfigMap
        let caInitEnv =
              merge
                { Some = \(_ : Text) ->
                    [ k8s.EnvVar::{ name = "POLAR_INIT_CA_CERT", value = Some "/etc/polar-ca/ca.pem" } ]
                , None = [] : List k8s.EnvVar.Type
                }
                v.caConfigMap
        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , k8s.Volume::{ name = "polar-health", emptyDir = Some k8s.EmptyDirVolumeSource::{=} }
              ] # caVolumes

        let env =
              Constants.commonClientEnv
              # [ k8s.EnvVar::{ name = "CREDENTIALS_AGENT_OBSERVER_PORT", value = Some (Natural/show v.port) }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_FILE",          value = Some "/var/run/polar-health/polar-health.json" }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_CERTS",         value = Some "/etc/tls/certs/cert.pem" }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_EXPIRY_SECS",   value = Some "60" }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS",     value = Some "30" }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_DEP_ENDPOINTS", value = Some "${Constants.cassiniDNSName}:${Natural/show Constants.cassiniPort}:300" }
                ]

        let mounts =
              [ Constants.certVolumeMount
              , k8s.VolumeMount::{ name = "polar-health", mountPath = "/var/run/polar-health" }
              ]

        let podSpec = k8s.PodSpec::{
              , imagePullSecrets = Some v.imagePullSecrets
              , serviceAccountName = Some v.name
              , automountServiceAccountToken = Some False
              , restartPolicy    = Some "Never"
              , volumes          = Some volumes
              , initContainers   = Some
                [ functions.makePolarInitContainer
                    v.polarInitImage
                    v.imagePullPolicy
                    Constants.saTokenVolumeName
                    v.certIssuerUrl
                    caInitEnv
                    Constants.saTokenPath
                    ([ k8s.VolumeMount::{ name = Constants.certVolumeName, mountPath = Constants.tlsPath } ] # caMounts)
                    ([] : List Text)
                    [ "client:${Constants.tlsPath}:ecdsa-p256:" ]
                    -- Same, real core-service exception as the
                    -- consumer half -- see deployment.dhall's own
                    -- comment for the complete reasoning.
                    True
                ]
              , containers =
                [ k8s.Container::{
                  , name            = v.name
                  , image           = Some v.image
                  , imagePullPolicy = Some v.imagePullPolicy
                  , securityContext = Some Constants.DropAllCapSecurityContext
                  , env             = Some env
                  , ports           = Some [ k8s.ContainerPort::{ containerPort = v.port } ]
                  , volumeMounts    = Some mounts
                  , livenessProbe   = Some k8s.Probe::{
                      , exec                = Some { command = Some [ "polar-healthcheck" ] }
                      , initialDelaySeconds = Some 150
                      , periodSeconds       = Some 30
                      , failureThreshold    = Some 2
                      , timeoutSeconds      = Some 5
                      }
                  }
                ]
              }

        let job =
              k8s.Job::{
              , metadata = k8s.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
              , spec = Some k8s.JobSpec::{
                , backoffLimit            = Some 1000000
                , parallelism             = Some 1
                , completions             = Some 1
                , ttlSecondsAfterFinished = Some 300
                , template                = k8s.PodTemplateSpec::{
                  , metadata = Some k8s.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                  , spec     = Some podSpec
                  }
                }
              }

        let service =
              k8s.Service::{
              , metadata = k8s.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace }
              , spec = Some k8s.ServiceSpec::{
                , selector = Some (toMap { name = v.name })
                , ports    = Some [ k8s.ServicePort::{ port = v.port, targetPort = Some (k8s.NatOrString.Nat v.port) } ]
                }
              }

        in  [ k8s.Resource.Service service, k8s.Resource.Job job ]

in render
