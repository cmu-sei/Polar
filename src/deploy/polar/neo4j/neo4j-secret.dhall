let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall


let token = kubernetes.Secret::{
apiVersion = "v1"
, kind = "Secret"
, metadata = kubernetes.ObjectMeta::{
    name = Some "neo4j-secret"
    , namespace = Some values.neo4j.namespace
    }
-- , data : Optional (List { mapKey : Text, mapValue : Text })
-- , immutable = Some True
, stringData = Some [ { mapKey = values.graphSecret.key, mapValue = env:NEO4J_AUTH as Text } ]
, type = Some "Opaque"
}

in token 