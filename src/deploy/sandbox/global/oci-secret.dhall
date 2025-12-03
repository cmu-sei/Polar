let kubernetes = ../../types/kubernetes.dhall
let Constants = ../../types/constants.dhall
let values = ../values.dhall
-- This is the secret to be used by flux to authenticate with our azure registry to download our deployment artifact
in kubernetes.Secret::{
  apiVersion = "v1"
  , kind = "Secret"
  , metadata = kubernetes.ObjectMeta::{
      , name = Some "flux-repo-secret"
      , namespace = Some Constants.PolarNamespace
    }
  -- NOTE:
  -- flux overwrites secrets as they get updated from the git repository, so we have to disable immutability.
  -- Enabling it is accepting the responsibility of keeping this secret manually updated, or through some other means.
  --, immutable = True
  , stringData = Some [
    , { mapKey = "username", mapValue = env:AZURE_CLIENT_ID as Text }
    , { mapKey = "password", mapValue = env:AZURE_CLIENT_SECRET as Text }
    ]
  , type = Some "Opaque"
}
