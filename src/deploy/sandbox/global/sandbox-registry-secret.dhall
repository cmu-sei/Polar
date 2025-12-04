

let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

let Constants = ../../types/constants.dhall
-- Secret used for image pulling
in kubernetes.Secret::{
  apiVersion = "v1"
, kind       = "Secret"
, metadata   = kubernetes.ObjectMeta::{
      name      = Some "sandbox-registry"
    , namespace = Some Constants.PolarNamespace
    }
, type       = Some "kubernetes.io/dockerconfigjson"
, data       = Some
    [ { mapKey = ".dockerconfigjson"
      , mapValue = env:OCI_REGISTRY_AUTH as Text
      }
    ]
}
