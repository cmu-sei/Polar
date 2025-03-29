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
      , serviceName = "neo4j-svc"
      , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
                name = Some "neo4j"
            ,   labels = Some [ { mapKey = "name", mapValue = "neo4j" } ]
            }
          , spec = Some kubernetes.PodSpec::{
            , containers =
              [ 
                kubernetes.Container::{
                , name = "neo4j"
                , image = Some values.neo4j.image
                , ports = Some
                  [ kubernetes.ContainerPort::{ containerPort = 7474 },
                    kubernetes.ContainerPort::{ containerPort = 7687 }
                  ]
                , volumeMounts = Some [
                    kubernetes.VolumeMount::{
                        name  = "neo4j-data"
                        , mountPath = "/var/lib/neo4j/conf"
                        
                    }
                ]
                },
              ]
            , volumes = Some [
                , kubernetes.Volume::{
                    , name = "neo4j-data"
                    , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                        claimName = "neo4j-pvc"
                    }
                }
            ]
            }
      }
      , replicas = Some 1
    }
  }

    
in  statefulSet