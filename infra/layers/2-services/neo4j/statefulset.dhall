-- infra/layers/2-services/neo4j/statefulset.dhall
--
-- Neo4j StatefulSet.
-- Init container: polar-nu-init:latest, executes /scripts/init.nu
-- which is mounted from the setup-neo4j-script ConfigMap.
-- No Alpine. No scripts passed as values. No env var imports.
--
-- TLS cert content comes from environment variables injected into the
-- init container at runtime via the pod spec env section.
-- The init container script (setup-neo4j.nu) reads them from $env.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let VolumeSpec =
      { name             : Text
      , storageClassName : Optional Text
      , storageSize      : Text
      , mountPath        : Text
      }

let render =
      \(v :
          { name            : Text
          , image           : Text
          , imagePullPolicy : Text
          , namespace       : Text
          , enableTls       : Bool
          , configVolume    : Text
          , ports           : { http : Natural, https : Natural, bolt : Natural }
          , config          : { name : Text, path : Text }
          , volumes         : { data : VolumeSpec, logs : VolumeSpec, certs : VolumeSpec }
          }
      ) ->

        let baseVolumeMounts =
              [ kubernetes.VolumeMount::{ name = v.configVolume,      mountPath = "/var/lib/neo4j/conf"   }
              , kubernetes.VolumeMount::{ name = v.volumes.data.name, mountPath = v.volumes.data.mountPath }
              , kubernetes.VolumeMount::{ name = v.volumes.logs.name, mountPath = v.volumes.logs.mountPath }
              , kubernetes.VolumeMount::{ name = "tmp",               mountPath = "/tmp"                   }
              ]

        let tlsVolumeMounts =
              if v.enableTls
              then [ kubernetes.VolumeMount::{ name = v.volumes.certs.name, mountPath = v.volumes.certs.mountPath } ]
              else [] : List kubernetes.VolumeMount.Type

        let initVolumeMounts =
              [ kubernetes.VolumeMount::{ name = Constants.neo4jConfigmapName, mountPath = "/config"             }
              , kubernetes.VolumeMount::{ name = v.configVolume,               mountPath = "/var/lib/neo4j/conf"  }
              , kubernetes.VolumeMount::{ name = "neo4j-init-script",          mountPath = "/scripts"             }
              ] # tlsVolumeMounts

        let baseEnv =
              [ kubernetes.EnvVar::{
                , name      = "NEO4J_AUTH"
                , valueFrom = Some kubernetes.EnvVarSource::{
                  , secretKeyRef = Some Constants.neo4jSecret
                  }
                }
              ]

        let tlsEnv =
              if v.enableTls
              then
                [ kubernetes.EnvVar::{ name = "TLS_SERVER_CERT_CONTENT", value = Some (env:NEO4J_TLS_SERVER_CERT_CONTENT as Text) }
                , kubernetes.EnvVar::{ name = "TLS_SERVER_KEY_CONTENT",  value = Some (env:NEO4J_TLS_SERVER_KEY_CONTENT  as Text) }
                , kubernetes.EnvVar::{ name = "TLS_CA_CERT_CONTENT",     value = Some (env:NEO4J_TLS_CA_CERT_CONTENT     as Text) }
                ]
              else [] : List kubernetes.EnvVar.Type

        let containerPorts =
                [ kubernetes.ContainerPort::{ containerPort = v.ports.http }
                , kubernetes.ContainerPort::{ containerPort = v.ports.bolt }
                ]
              # ( if v.enableTls
                  then [ kubernetes.ContainerPort::{ containerPort = v.ports.https } ]
                  else [] : List kubernetes.ContainerPort.Type
                )

        let baseVolumes =
              [ kubernetes.Volume::{
                , name                  = v.volumes.data.name
                , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = v.volumes.data.name }
                }
              , kubernetes.Volume::{
                , name                  = v.volumes.logs.name
                , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = v.volumes.logs.name }
                }
              , kubernetes.Volume::{
                , name      = v.config.name
                , configMap = Some kubernetes.ConfigMapVolumeSource::{
                  , name  = Some v.config.name
                  , items = Some [ kubernetes.KeyToPath::{ key = "neo4j.conf", path = "neo4j.conf" } ]
                  }
                }
              , kubernetes.Volume::{ name = v.configVolume, emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              , kubernetes.Volume::{ name = "tmp",          emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              , kubernetes.Volume::{
                , name      = "neo4j-init-script"
                , configMap = Some kubernetes.ConfigMapVolumeSource::{
                  , name  = Some "neo4j-init-script"
                  , items = Some [ kubernetes.KeyToPath::{ key = "init.nu", path = "init.nu" } ]
                  }
                }
              ]

        let tlsVolumes =
              if v.enableTls
              then
                [ kubernetes.Volume::{
                  , name                  = v.volumes.certs.name
                  , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = v.volumes.certs.name }
                  }
                ]
              else [] : List kubernetes.Volume.Type

        in  kubernetes.StatefulSet::{
            , apiVersion = "apps/v1"
            , kind       = "StatefulSet"
            , metadata   = kubernetes.ObjectMeta::{
              , name      = Some v.name
              , namespace = Some v.namespace
              }
            , spec = Some kubernetes.StatefulSetSpec::{
              , selector    = kubernetes.LabelSelector::{
                , matchLabels = Some (toMap { name = v.name })
                }
              , serviceName = Constants.neo4jServiceName
              , replicas    = Some 1
              , template    = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{
                  , name   = Some v.name
                  , labels = Some [ { mapKey = "name", mapValue = v.name } ]
                  }
                , spec = Some kubernetes.PodSpec::{
                  , securityContext = Some kubernetes.PodSecurityContext::{
                    , fsGroup             = Some 7474
                    , fsGroupChangePolicy = Some "OnRootMismatch"
                    }
                  , initContainers = Some
                    [ kubernetes.Container::{
                      , name            = "neo4j-init"
                      , image           = Some "polar-nu-init:latest"
                      , imagePullPolicy = Some v.imagePullPolicy
                      , env             = Some (baseEnv # tlsEnv)
                      , volumeMounts    = Some initVolumeMounts
                      }
                    ]
                  , containers =
                    [ kubernetes.Container::{
                      , name            = v.name
                      , image           = Some v.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some kubernetes.SecurityContext::{
                        , runAsGroup   = Some 7474
                        , runAsNonRoot = Some True
                        , runAsUser    = Some 7474
                        , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
                        }
                      , env          = Some (baseEnv # tlsEnv)
                      , ports        = Some containerPorts
                      , volumeMounts = Some (baseVolumeMounts # tlsVolumeMounts)
                      }
                    ]
                  , volumes = Some (baseVolumes # tlsVolumes)
                  }
                }
              }
            }

in render
