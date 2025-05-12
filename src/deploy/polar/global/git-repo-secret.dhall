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
  -- NOTE: 
  -- flux overwrites secrets as they get updated from the git repository, so we have to disable immutability.
  -- Enabling it is accepting the responsibility of keeping this secret manually updated, or through some other means.
  --, immutable = True
  , stringData = Some [
    , { mapKey = "username", mapValue = env:GITLAB_USER as Text } 
    , { mapKey = "password", mapValue = env:FLUX_GIT_REPO_TOKEN as Text } 
    ]
  , type = Some "Opaque"
}

in secret 