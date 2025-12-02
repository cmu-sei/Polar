let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall


let token = kubernetes.Secret::{
apiVersion = "v1"
, kind = "Secret"
, metadata = kubernetes.ObjectMeta::{
    name = Some "polar-graph-pw"
    , namespace = Some values.namespace
    }
-- , data : Optional (List { mapKey : Text, mapValue : Text })
-- , immutable = Some True
, stringData = Some [ { mapKey = values.graphSecret.key, mapValue = env:GRAPH_PASSWORD as Text } ]
, type = Some "Opaque"
}

in token 