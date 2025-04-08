
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let neo4jVolumeClaim = kubernetes.PersistentVolumeClaim::{
    metadata = kubernetes.ObjectMeta::{
        name = Some values.neo4j.volumes.data.name
        , namespace = Some values.neo4j.namespace
    }
    , spec = Some kubernetes.PersistentVolumeClaimSpec::{ 
        selector = Some kubernetes.LabelSelector::{ matchLabels = Some (toMap { name = "neo4j" }) }
        , accessModes = Some [ "ReadWriteOnce" ]
        , volumeName = Some values.neo4j.volumes.data.name
        , resources = Some kubernetes.VolumeResourceRequirements::{
            requests = Some ([
               { mapKey = "storage", mapValue = values.neo4j.volumes.data.storageSize}
            ])
          }
        , storageClassName = values.neo4j.volumes.data.storageClassName

    }
}

in neo4jVolumeClaim