let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let secret = kubernetes.Secret::{
apiVersion = "v1"
, kind = "Secret"
, metadata = kubernetes.ObjectMeta::{
    name = Some "flux-repo-secret"
    , namespace = Some values.namespace
    }
-- , data : Optional (List { mapKey : Text, mapValue : Text })\
-- In the future, it'll be desirable to use a seperate gitlab token than one for a user
, immutable = Some True
, stringData = Some [
  { mapKey = "username", mapValue = env:GITLAB_USER as Text } 
  , { mapKey = "password", mapValue = env:GITLAB_TOKEN as Text } ]

, type = Some "Opaque"
}

in secret 