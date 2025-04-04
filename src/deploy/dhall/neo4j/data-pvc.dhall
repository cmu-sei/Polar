
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let neo4jVolumeClaim = kubernetes.PersistentVolumeClaim::{
    metadata = kubernetes.ObjectMeta::{
        name = Some "neo4j-data-pvc"
        , namespace = Some "polar"
    }
    , spec = Some kubernetes.PersistentVolumeClaimSpec::{ 
        selector = Some kubernetes.LabelSelector::{ matchLabels = Some (toMap { name = "neo4j" }) }
        , accessModes = Some [ "ReadWriteOnce" ]
        , volumeName = Some "neo4j-data"
        , resources = Some kubernetes.VolumeResourceRequirements::{
            requests = Some ([
               { mapKey = "storage", mapValue = "10Gi"}
            ])
          }
        , storageClassName = Some "standard"

    }
}

in neo4jVolumeClaim