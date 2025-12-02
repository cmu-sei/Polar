let kubernetes = ../types/kubernetes.dhall

let Constants = ../types/constants.dhall
let values = ./values.dhall
let setupScript = ../../../scripts/setup-neo4j.sh as Text
let configContent = ../../conf/neo4j-no-ssl.conf as Text

let instanceName = "polar-neo4j"
let namespace =
      kubernetes.Namespace::{
      , apiVersion = "v1"
      , kind = "Namespace"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.GraphNamespace
        }
      }

let configMap =
      kubernetes.ConfigMap::{
      , apiVersion = "v1"
      , kind = "ConfigMap"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.neo4jConfigmapName
        , namespace = Some Constants.GraphNamespace
        }
      , data = Some [ { mapKey = "neo4j.conf", mapValue = configContent } ]
      }

let secret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "neo4j-secret"
        , namespace = Some values.neo4j.namespace
        }
      , stringData = Some
        [ { mapKey = Constants.neo4jSecret.key
          , mapValue = env:NEO4J_AUTH as Text
          }
        ]
      , type = Some "Opaque"
      }

let logVolume =
      kubernetes.PersistentVolume::{
      , apiVersion = "v1"
      , kind = "PersistentVolume"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.neo4j.volumes.logs.name
        , namespace = Some Constants.GraphNamespace
        }
      , spec = Some kubernetes.PersistentVolumeSpec::{
        , accessModes = Some [ "ReadWriteOnce" ]
        , capacity = Some [ { mapKey = "storage", mapValue = "10Gi" } ]
        , hostPath = Some kubernetes.HostPathVolumeSource::{
          , path = "/tmp/neo4j/logs"
          }
        , storageClassName = Some "standard"
        }
      }

let dataVolume =
      kubernetes.PersistentVolume::{
      , apiVersion = "v1"
      , kind = "PersistentVolume"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.neo4j.volumes.data.name
        , namespace = Some values.neo4j.namespace
        }
      , spec = Some kubernetes.PersistentVolumeSpec::{
        , accessModes = Some [ "ReadWriteOnce" ]
        , capacity = Some [ { mapKey = "storage", mapValue = "10Gi" } ]
        , hostPath = Some kubernetes.HostPathVolumeSource::{
          , path = "/tmp/neo4j/data"
          }
        , storageClassName = Some "standard"
        }
      }

let logVolumeClaim =
      kubernetes.PersistentVolumeClaim::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.neo4j.volumes.logs.name
        , namespace = Some values.neo4j.namespace
        }
      , spec = Some kubernetes.PersistentVolumeClaimSpec::{
        , accessModes = Some [ "ReadWriteOnce" ]
        , resources = Some kubernetes.VolumeResourceRequirements::{
          , requests = Some
            [ { mapKey = "storage"
              , mapValue = values.neo4j.volumes.logs.storageSize
              }
            ]
          }
        , storageClassName = values.neo4j.volumes.logs.storageClassName
        }
      }

let dataVolumeClaim =
      kubernetes.PersistentVolumeClaim::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.neo4j.volumes.data.name
        , namespace = Some values.neo4j.namespace
        }
      , spec = Some kubernetes.PersistentVolumeClaimSpec::{
        , accessModes = Some [ "ReadWriteOnce" ]
        , resources = Some kubernetes.VolumeResourceRequirements::{
          , requests = Some
            [ { mapKey = "storage"
              , mapValue = values.neo4j.volumes.data.storageSize
              }
            ]
          }
        , storageClassName = values.neo4j.volumes.data.storageClassName
        }
      }

let serviceSpec =
      kubernetes.ServiceSpec::{
      , selector = Some (toMap { name = instanceName })
      , type = Some "NodePort"
      , ports = Some
        [ kubernetes.ServicePort::{
          , name = Some "http-ui"
          , protocol = Some "TCP"
          , targetPort = Some
              (kubernetes.NatOrString.Nat values.neo4j.ports.http)
          , port = values.neo4j.ports.http
          }
        , kubernetes.ServicePort::{
          , name = Some "bolt"
          , protocol = Some "TCP"
          , appProtocol = Some "kubernetes.io/ws"
          , targetPort = Some
              (kubernetes.NatOrString.Nat values.neo4j.ports.bolt)
          , port = values.neo4j.ports.bolt
          }
        ]
      }

let service
    : kubernetes.Service.Type
    = kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.neo4jServiceName
        , namespace = Some Constants.GraphNamespace
        }
      , spec = Some serviceSpec
      }


let podSpec =
      kubernetes.PodSpec::{
      , initContainers = Some
        [ kubernetes.Container::{
          , name = "neo4j-init"
          , image = Some "alpine:3.14.0"
          , command = Some [ "/bin/sh", "-c" ]
          , args = Some [ setupScript ]
          , volumeMounts = Some
            [ kubernetes.VolumeMount::{
              , name = Constants.neo4jConfigmapName
              , mountPath = "/config"
              }
            , kubernetes.VolumeMount::{
              , name = values.neo4j.configVolume
              , mountPath = "/var/lib/neo4j/conf"
              }
            ]
          }
        ]
        , securityContext = Some kubernetes.PodSecurityContext::{
        , fsGroup = Some 7474
        , fsGroupChangePolicy = Some "OnRootMismatch"
        }
      , containers =
        [ kubernetes.Container::{
          , name = values.neo4j.name
          , image = Some values.neo4j.image

        , securityContext = Some kubernetes.SecurityContext::{
        , runAsGroup = Some 7474
        , runAsNonRoot = Some True
        , runAsUser = Some 7474
        , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
        }
          , env = Some
            [ kubernetes.EnvVar::{
              , name = "NEO4J_AUTH"
              , valueFrom = Some kubernetes.EnvVarSource::{
                , secretKeyRef = Some Constants.neo4jSecret
                }
              }
            ]
          , ports = Some
            [ kubernetes.ContainerPort::{
              , containerPort = values.neo4j.ports.http
              }
            , kubernetes.ContainerPort::{
              , containerPort = values.neo4j.ports.bolt
              }
            ]
          , volumeMounts = Some
            [ kubernetes.VolumeMount::{
              , name = values.neo4j.configVolume
              , mountPath = "/var/lib/neo4j/conf"
              }
            , kubernetes.VolumeMount::{
              , name = values.neo4j.volumes.data.name
              , mountPath = values.neo4j.volumes.data.mountPath
              }
            , kubernetes.VolumeMount::{
              , name = values.neo4j.volumes.logs.name
              , mountPath = values.neo4j.volumes.logs.mountPath
              }
            ]
          }
        ]
      , volumes = Some
        [ kubernetes.Volume::{
          , name = values.neo4j.volumes.data.name
          , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
            , claimName = values.neo4j.volumes.data.name
            }
          }
        , kubernetes.Volume::{
          , name = values.neo4j.volumes.logs.name
          , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
            , claimName = values.neo4j.volumes.logs.name
            }
          }
        , kubernetes.Volume::{
          , name = values.neo4j.config.name
          , configMap = Some kubernetes.ConfigMapVolumeSource::{
            , name = Some values.neo4j.config.name
            , items = Some
              [ kubernetes.KeyToPath::{
                , key = "neo4j.conf"
                , path = "neo4j.conf"
                }
              ]
            }
          }
        , kubernetes.Volume::{
          , name = values.neo4j.configVolume
          , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
          }
        ]
      }

let statefulSet =
      kubernetes.StatefulSet::{
      , apiVersion = "apps/v1"
      , kind = "StatefulSet"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.neo4j.name
        , namespace = Some Constants.GraphNamespace
        }
      , spec = Some kubernetes.StatefulSetSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = values.neo4j.name })
          }
        , serviceName = Constants.neo4jServiceName
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name = Some values.neo4j.name
            , labels = Some
              [ { mapKey = "name", mapValue = values.neo4j.name } ]
            }
          , spec = Some podSpec
          }
        , replicas = Some 1
        }
      }

in  [ kubernetes.Resource.Namespace namespace
    , kubernetes.Resource.ConfigMap configMap
    , kubernetes.Resource.Secret secret
    , kubernetes.Resource.PersistentVolume logVolume
    , kubernetes.Resource.PersistentVolume dataVolume
    , kubernetes.Resource.PersistentVolumeClaim dataVolumeClaim
    , kubernetes.Resource.PersistentVolumeClaim logVolumeClaim
    , kubernetes.Resource.Service service
    , kubernetes.Resource.StatefulSet statefulSet
    ]
