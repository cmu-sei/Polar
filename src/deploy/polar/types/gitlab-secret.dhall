let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

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