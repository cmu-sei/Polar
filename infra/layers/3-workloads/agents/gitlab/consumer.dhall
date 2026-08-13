-- infra/layers/3-workloads/agents/gitlab/consumer.dhall
--
-- GitLab consumer Job.
-- Reads Cassini topics and writes to the graph.

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
          , neo4jBoltAddr    : Text
          , proxyCACert      : Optional Text
          }
      ) ->
        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , kubernetes.Volume::{ name = Constants.neo4jClientCertVolumeName, emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              , kubernetes.Volume::{ name = "polar-health", emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              ] # functions.ProxyVolume v.proxyCACert

        let env =
              Constants.commonClientEnv
              # functions.makeGraphEnv v.neo4jBoltAddr Constants.graphConfig Constants.graphSecretKeySelector (Some "/etc/neo4j-client-tls/ca.pem")
              # [ kubernetes.EnvVar::{ name = "POLAR_HEALTH_FILE",        value = Some "/var/run/polar-health/polar-health.json" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_CERTS",       value = Some "/etc/tls/certs/cert.pem,/etc/neo4j-client-tls/cert.pem" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_EXPIRY_SECS", value = Some "60" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS",   value = Some "30" }
                ]

        let mounts =
              [ Constants.certVolumeMount
              , kubernetes.VolumeMount::{ name = Constants.neo4jClientCertVolumeName, mountPath = Constants.neo4jClientCertPath, readOnly = Some True }
              , kubernetes.VolumeMount::{ name = "polar-health", mountPath = "/var/run/polar-health" }
              ] # functions.ProxyMount v.proxyCACert

        in  kubernetes.Job::{
            , metadata = kubernetes.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.JobSpec::{
              , backoffLimit            = Some 1000000
              , parallelism             = Some 1
              , completions             = Some 1
              , ttlSecondsAfterFinished = Some 300
              , template                = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                , spec     = Some kubernetes.PodSpec::{
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
                        [ kubernetes.VolumeMount::{ name = Constants.certVolumeName,            mountPath = Constants.tlsPath             }
                        , kubernetes.VolumeMount::{ name = Constants.neo4jClientCertVolumeName, mountPath = Constants.neo4jClientCertPath }
                        ]
                        ([] : List Text)
                        [ "client:${Constants.tlsPath}:ecdsa-p256:"
                        , "client:${Constants.neo4jClientCertPath}:ecdsa-p256:"
                        ]
                    ]
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
