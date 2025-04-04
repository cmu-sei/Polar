let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let statefulSet = 
    kubernetes.StatefulSet::{ 
      apiVersion = "apps/v1"
    , kind = "StatefulSet"
    , metadata = kubernetes.ObjectMeta::{
        name = Some values.neo4j.name
        , namespace = Some values.namespace
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
            }
          , spec = Some kubernetes.PodSpec::{
            , containers =
              [ 
                kubernetes.Container::{
                , name = values.neo4j.name
                , image = Some values.neo4j.image
                , env = Some values.neo4j.env
                , securityContext = Some values.neo4j.containerSecurityContext
                , ports = Some
                  [ kubernetes.ContainerPort::{ containerPort = 7474 },
                    kubernetes.ContainerPort::{ containerPort = 7687 }
                  ]
                , volumeMounts = Some [
                    kubernetes.VolumeMount::{
                        name  = values.neo4j.volumes.data.name
                        , mountPath = values.neo4j.volumes.data.mountPath
                    }
                    ,kubernetes.VolumeMount::{
                        name  = values.neo4j.config.name
                        , mountPath = values.neo4j.config.path
                        , readOnly = Some True 
                    }
                    ,kubernetes.VolumeMount::{
                        name  = values.neo4j.volumes.logs.name
                        , mountPath = values.neo4j.volumes.logs.mountPath
                    }
                ]
                },
              ]
            , volumes = Some [
                , kubernetes.Volume::{
                    , name = "neo4j-data"
                    , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                        claimName = "neo4j-data-pvc"
                    }
                }
                , kubernetes.Volume::{
                    , name = "neo4j-logs"
                    , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                        claimName = "neo4j-logs-pvc"
                    }
                }
                ,kubernetes.Volume::{
                  , name = values.neo4j.config.name 
                  , configMap = Some kubernetes.ConfigMapVolumeSource::{
                    name = Some values.neo4j.config.name
                    , items = Some [ kubernetes.KeyToPath::{ key = "neo4j.conf", path = "neo4j.conf" } ]
                    }
                }
            ]
            }
      }
      , replicas = Some 1
    }
  }

    
in  statefulSet