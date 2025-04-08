let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let neo4jDataVolume = kubernetes.PersistentVolume::{ 
    apiVersion = "v1"
    , kind = "PersistentVolume"
    , metadata = kubernetes.ObjectMeta::{
        name = Some values.neo4j.volumes.logs.name
        , namespace = Some values.neo4j.namespace
    }
    , spec = Some kubernetes.PersistentVolumeSpec::{
        accessModes = Some ["ReadWriteOnce"]
        , capacity = 
            Some ([
                { mapKey = "storage"
                , mapValue = "10Gi"
                }
            ])
        , hostPath = Some kubernetes.HostPathVolumeSource:: { path = "/data/neo4j/logs" }
        , storageClassName = Some "standard"
    }    
}

in neo4jDataVolume
