
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let Namespace = kubernetes.Namespace::{
    apiVersion = "v1"
    , kind = "Namespace"
    , metadata = kubernetes.ObjectMeta::{
        name = Some values.neo4j.namespace
    }
}

in Namespace