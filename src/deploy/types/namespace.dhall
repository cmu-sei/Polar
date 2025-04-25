
let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

let Namespace = kubernetes.Namespace::{
    apiVersion = "v1"
    , kind = "Namespace"
    , metadata = kubernetes.ObjectMeta::{
        name = Some values.namespace
    }
}

in Namespace