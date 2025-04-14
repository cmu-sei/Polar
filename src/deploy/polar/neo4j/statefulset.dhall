let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let emptyDirVolumeName = "neo4j-conf-copy"

let spec 
  = kubernetes.PodSpec::{
    , securityContext = Some values.neo4j.podSecurityContext
    , imagePullSecrets = Some values.sandboxRegistry.imagePullSecrets
    , initContainers = Some [
      kubernetes.Container::{
        name = "copy-neo4j-config"
        , image = Some "${values.sandboxRegistry.url}/busybox:1.35.0"
        , command = Some [ "/bin/sh", "-c" ]
        , args = Some [ "cp /config/neo4j.conf /var/lib/neo4j/conf/neo4j.conf" ]
        , securityContext = Some values.neo4j.containerSecurityContext
        , volumeMounts = Some [
          -- mount the read-only config
          , kubernetes.VolumeMount::{
              name  = values.neo4j.config.name
              , mountPath = "/config"
            }
            -- Mount empty dir to copy into
          , kubernetes.VolumeMount::{
            name  = emptyDirVolumeName
            , mountPath = values.neo4j.config.path
          } 
        ]
      }
    ]
    , containers =
      [ 
        kubernetes.Container::{
        , name = values.neo4j.name
        , image = Some values.neo4j.image
        , env = Some values.neo4j.env
        , securityContext = Some values.neo4j.containerSecurityContext
        , ports = Some values.neo4j.containerPorts
        , volumeMounts = Some [
            kubernetes.VolumeMount::{
                name  = values.neo4j.volumes.data.name
                , mountPath = values.neo4j.volumes.data.mountPath
            }
            -- mount populated dir with writeable config
            ,kubernetes.VolumeMount::{
                name  = emptyDirVolumeName
                , mountPath = values.neo4j.config.path
            }
            ,kubernetes.VolumeMount::{
                name  = values.neo4j.volumes.logs.name
                , mountPath = values.neo4j.volumes.logs.mountPath
            }
            ,kubernetes.VolumeMount::{
                name  = values.neo4j.tls.leafSecretName
                , mountPath = values.neo4j.tls.httpsMountPath
            }
            ,kubernetes.VolumeMount::{
                name  = values.neo4j.tls.leafSecretName
                , mountPath = values.neo4j.tls.boltMountPath
            }
        ]
        },
      ]
    , volumes = Some [
        , kubernetes.Volume::{
            , name = values.neo4j.volumes.data.name
            , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                claimName = values.neo4j.volumes.data.name
            }
        }
        , kubernetes.Volume::{
            , name = values.neo4j.volumes.logs.name
            , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                claimName = values.neo4j.volumes.logs.name
            }
        }
        , kubernetes.Volume::{
            , name = values.neo4j.tls.leafSecretName
            , secret = Some kubernetes.SecretVolumeSource::{
                secretName = Some values.neo4j.tls.leafSecretName
            }
        } 
        -- Our initial read-only neo4j configmap 
        , kubernetes.Volume::{
          , name = values.neo4j.config.name 
          , configMap = Some kubernetes.ConfigMapVolumeSource::{
            name = Some values.neo4j.config.name
            , items = Some [ kubernetes.KeyToPath::{ key = "neo4j.conf", path = "neo4j.conf" } ]
            }
        }
        -- Our shared, writeable emptyDir volume to copy the config to so neo4j can write to it
        , kubernetes.Volume::{
          name = emptyDirVolumeName
          , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
        }
    ]
  }
let statefulSet = 
    kubernetes.StatefulSet::{ 
      apiVersion = "apps/v1"
    , kind = "StatefulSet"
    , metadata = kubernetes.ObjectMeta::{
        name = Some values.neo4j.name
        , namespace = Some values.neo4j.namespace
      }
    , spec = Some kubernetes.StatefulSetSpec::{ 
      selector = kubernetes.LabelSelector::{
        matchLabels = Some (toMap { name = values.neo4j.name })
      }
      , serviceName = values.neo4j.service.name
      , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
                name = Some values.neo4j.name
            ,   labels = Some [ { mapKey = "name", mapValue = values.neo4j.name } ]
            , annotations = Some values.neo4j.podAnnotations
            }
          , spec = Some spec 
      }
      , replicas = Some 1
    }
  }

    
in  statefulSet