let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let neo4jDataVolume = kubernetes.PersistentVolume::{ 
    apiVersion = "v1"
    , kind = "PersistentVolume"
    , metadata = kubernetes.ObjectMeta::{
        name = Some "neo4j-data"
        , namespace = Some "polar"
    }
    , spec = Some kubernetes.PersistentVolumeSpec::{
        accessModes = Some values.neo4j.volumes.data.selector.accessModes
        , capacity = 
            Some ([
                { mapKey = "storage"
                , mapValue = values.neo4j.volumes.data.selector.requests.storage
                }
            ])
        , hostPath = Some kubernetes.HostPathVolumeSource:: { path = "/data/conf" }
        , storageClassName = Some values.neo4j.volumes.data.selector.storageClassName
    }    
}

in neo4jDataVolume
