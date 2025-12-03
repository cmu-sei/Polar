let kubernetes = ../../types/kubernetes.dhall
let Constants = ../../types/constants.dhall
let values = ../values.dhall
-- This is the secret to be used by flux to authenticate with our azure registry to download our deployment artifact
-- All in all, it's probably easier to just write down the command, but better that we know the format ahead of time.
in kubernetes.Secret::{
  apiVersion = "v1"
, kind       = "Secret"
, metadata   = kubernetes.ObjectMeta::{
    , name = Some "flux-repo-secret"
    , namespace = Some Constants.PolarNamespace
  }
, type       = Some "kubernetes.io/dockerconfigjson"
, data       = Some
    [ { mapKey = ".dockerconfigjson"
      , mapValue = "REPLACE_WITH_BASE64"  -- you insert final base64 string here
      }
    ]
}
