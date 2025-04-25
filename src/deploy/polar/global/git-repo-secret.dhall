let kubernetes = ../../types/kubernetes.dhall

let values = ../values.dhall
-- This is the secret to be used by flux to authenticate with our gitlab instance to download our helm chart
let secret = kubernetes.Secret::{
  apiVersion = "v1"
  , kind = "Secret"
  , metadata = kubernetes.ObjectMeta::{
      , name = Some "flux-repo-secret" 
      , namespace = Some values.namespace
    }
  -- TODO: make the secret immutable, nothing should be touching it, it should only ever be deleted and then recreated
  --, immutable = True
  -- In the future, it'll be desirable to use a seperate gitlab token than one for a user
  , stringData = Some [
    , { mapKey = "username", mapValue = env:GITLAB_USER as Text } 
    , { mapKey = "password", mapValue = env:GITLAB_TOKEN as Text } 
    ]
  , type = Some "Opaque"
}

in secret 