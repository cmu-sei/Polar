let kubernetes = ../../types/kubernetes.dhall
let Constants = ../../types/constants.dhall
let values = ../values.dhall

let namespace = kubernetes.Namespace::{
      , apiVersion = "v1"
      , kind = "Namespace"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.PolarNamespace
        }
      }
-- TODO:
-- This should probably be a constant
let neo4jCredentialSecret = kubernetes.Secret::{
apiVersion = "v1"
, kind = "Secret"
, metadata = kubernetes.ObjectMeta::{
    name = Some "polar-graph-pw"
    , namespace = Some Constants.PolarNamespace
    }
-- , data : Optional (List { mapKey : Text, mapValue : Text })
-- , immutable = Some True
, stringData = Some [ { mapKey = Constants.neo4jSecret.key, mapValue = env:GRAPH_PASSWORD as Text } ]
, type = Some "Opaque"
}

let ociSecret = env:DOCKER_CONFIG_STR as Text

-- Secret used for image pulling
let imagePullSecret = kubernetes.Secret::{
  apiVersion = "v1"
, kind       = "Secret"
, metadata   = kubernetes.ObjectMeta::{
      name      = Some Constants.imagePullSecretName
    , namespace = Some Constants.PolarNamespace
    }
, type       = Some "kubernetes.io/dockerconfigjson"
, data       = Some
    [ { mapKey = ".dockerconfigjson"
      , mapValue = ociSecret
      }
    ]
}


in [ kubernetes.Resource.Namespace namespace, kubernetes.Resource.Secret neo4jCredentialSecret, kubernetes.Resource.Secret imagePullSecret ]
