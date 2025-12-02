
let kubernetes = ../../types/kubernetes.dhall
let Constants = ./constants.dhall

in
kubernetes.Namespace::{
    apiVersion = "v1"
    , kind = "Namespace"
    , metadata = kubernetes.ObjectMeta::{
        name = Some Constants.PolarNamespace
    }
}
