
let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

let neo4jVolumeClaim = kubernetes.PersistentVolumeClaim::{
    metadata = kubernetes.ObjectMeta::{
        name = Some values.neo4j.volumes.logs.name
        , namespace = Some values.neo4j.namespace
    }
    , spec = Some kubernetes.PersistentVolumeClaimSpec::{ 
        , accessModes = Some [ "ReadWriteOnce" ]
        -- , volumeName = Some values.neo4j.volumes.logs.name
        , resources = Some kubernetes.VolumeResourceRequirements::{
            requests = Some ([
               { mapKey = "storage", mapValue = values.neo4j.volumes.logs.storageSize}
            ])
          }
        , storageClassName = values.neo4j.volumes.logs.storageClassName

    }
}

in neo4jVolumeClaim