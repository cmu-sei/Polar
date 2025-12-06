let kubernetes = ../../types/kubernetes.dhall
let Constants = ../../types/constants.dhall
let Functions = ../../types/functions.dhall

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

in [ kubernetes.Resource.Namespace namespace, kubernetes.Resource.Secret neo4jCredentialSecret ]
