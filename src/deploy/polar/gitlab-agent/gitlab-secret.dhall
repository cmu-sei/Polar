let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall


let token = kubernetes.Secret::{
apiVersion = "v1"
, kind = "Secret"
, metadata = kubernetes.ObjectMeta::{
    name = Some values.gitlab.name
    , namespace = Some values.namespace
    }
-- , data : Optional (List { mapKey : Text, mapValue : Text })
, immutable = Some True
, stringData = Some [ { mapKey = values.gitlabSecret.key, mapValue = env:GITLAB_TOKEN as Text } ]
, type = Some "Opaque"
}

in token 