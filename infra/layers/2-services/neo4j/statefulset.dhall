-- infra/layers/2-services/neo4j/statefulset.dhall
--
-- Neo4j StatefulSet.
-- Init containers (in order):
--   1. cert-client: requests a server TLS cert from the cert-issuer,
--      writes cert.pem, key.pem, ca.pem to the neo4j-tls emptyDir.
--   2. neo4j-init: reads certs from neo4j-tls, writes them into the
--      neo4j certificate directory structure, and copies neo4j.conf.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall
let Functions = ../../../schema/functions.dhall

let VolumeSpec =
      { name             : Text
      , storageClassName : Optional Text
      , storageSize      : Text
      , mountPath        : Text
      }

let render =
      \(v :
          { name                : Text
          , image               : Text
          , imagePullPolicy     : Text
          , namespace           : Text
          , configVolume        : Text
          , certIssuerUrl       : Text
          , certClientImage     : Text
          , certIssuerAudience  : Text
          , neo4jSans           : List Text
          , ports               : { http : Natural, https : Natural, bolt : Natural }
          , config              : { name : Text, path : Text }
          , volumes             : { data : VolumeSpec, logs : VolumeSpec, certs : VolumeSpec }
          }
      ) ->

        let neo4jTlsVolume =
        kubernetes.Volume::{
        , name     = "neo4j-tls"
        , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
        }

        let neo4jSaTokenVolume =
        kubernetes.Volume::{
        , name = "neo4j-sa-token"
        , projected = Some kubernetes.ProjectedVolumeSource::{
          , sources = Some
            [ kubernetes.VolumeProjection::{
              , serviceAccountToken = Some kubernetes.ServiceAccountTokenProjection::{
                , path     = "token"
                , audience = Some v.certIssuerAudience
                , expirationSeconds = Some 3600
                }
              }
            ]
          }
        }

        let baseVolumeMounts =
              [ kubernetes.VolumeMount::{ name = v.configVolume,       mountPath = "/var/lib/neo4j/conf"           }
              , kubernetes.VolumeMount::{ name = v.volumes.data.name,  mountPath = v.volumes.data.mountPath        }
              , kubernetes.VolumeMount::{ name = v.volumes.logs.name,  mountPath = v.volumes.logs.mountPath        }
              , kubernetes.VolumeMount::{ name = v.volumes.certs.name, mountPath = v.volumes.certs.mountPath       }
              , kubernetes.VolumeMount::{ name = "tmp",                mountPath = "/tmp"                          }
              ]

        let initVolumeMounts =
              [ kubernetes.VolumeMount::{ name = Constants.neo4jConfigmapName, mountPath = "/config"                    }
              , kubernetes.VolumeMount::{ name = v.configVolume,               mountPath = "/var/lib/neo4j/conf"         }
              , kubernetes.VolumeMount::{ name = "neo4j-init-script",          mountPath = "/scripts"                    }
              , kubernetes.VolumeMount::{ name = v.volumes.certs.name,         mountPath = v.volumes.certs.mountPath     }
              , kubernetes.VolumeMount::{ name = "neo4j-tls",                  mountPath = "/etc/neo4j-tls", readOnly = Some True }
              ]

        let baseEnv =
              [ kubernetes.EnvVar::{
                , name      = "NEO4J_AUTH"
                , valueFrom = Some kubernetes.EnvVarSource::{
                  , secretKeyRef = Some Constants.neo4jSecret
                  }
                }
              ]

        let containerPorts =
              [ kubernetes.ContainerPort::{ containerPort = v.ports.http  }
              , kubernetes.ContainerPort::{ containerPort = v.ports.bolt  }
              , kubernetes.ContainerPort::{ containerPort = v.ports.https }
              ]

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
              , kubernetes.Volume::{
                , name                  = v.volumes.certs.name
                , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = v.volumes.certs.name }
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
                    [ Functions.makeCertServerInitContainer v.certIssuerUrl v.certClientImage v.neo4jSans
                    , kubernetes.Container::{
                      , name            = "neo4j-init"
                      , image           = Some "polar-nu-init:latest"
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some kubernetes.SecurityContext::{
                        , runAsUser    = Some 0
                        , runAsGroup   = Some 0
                        , runAsNonRoot = Some False
                        }
                      , env          = Some baseEnv
                      , volumeMounts = Some initVolumeMounts
                      }
                    , kubernetes.Container::{
                      , name            = "neo4j-cacerts"
                      , image           = Some v.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some kubernetes.SecurityContext::{
                        , runAsUser    = Some 0
                        , runAsGroup   = Some 0
                        , runAsNonRoot = Some False
                        }
                      , command = Some [ "/bin/sh", "-c" ]
                      , args = Some
                      [ "JAVA_HOME=$(dirname $(dirname $(readlink -f /bin/java))) && cp $JAVA_HOME/lib/security/cacerts /var/lib/neo4j/conf/cacerts && chmod u+w /var/lib/neo4j/conf/cacerts && /bin/keytool -import -noprompt -alias polar-internal-ca -file /var/lib/neo4j/certificates/https/trusted/ca.pem -keystore /var/lib/neo4j/conf/cacerts -storepass changeit"
                      ]
                      , volumeMounts = Some
                        [ kubernetes.VolumeMount::{ name = v.configVolume,       mountPath = "/var/lib/neo4j/conf"        }
                        , kubernetes.VolumeMount::{ name = v.volumes.certs.name, mountPath = v.volumes.certs.mountPath     }
                        ]
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
                      , env          = Some (baseEnv)
                      , ports        = Some containerPorts
                      , volumeMounts = Some (baseVolumeMounts)
                      }
                    ]
                  , volumes = Some (baseVolumes # [ neo4jTlsVolume, neo4jSaTokenVolume ])
                  }
                }
              }
            }

in render
