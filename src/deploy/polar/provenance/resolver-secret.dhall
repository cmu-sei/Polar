-- This creates a config.json secret to be used to authenticate with any preferred registries
-- this secret will be mounted as part of the deployment for the resolver agent

let kubernetes = ../../types/kubernetes.dhall

let Constants = ../../types/Constants.dhall

in  kubernetes.Secret::{
    , apiVersion = "v1"
    , kind = "Secret"
    , metadata = kubernetes.ObjectMeta::{
      , name = Some Constants.OciRegistrySecret.name
      , namespace = Some Constants.PolarNamespace
      }
    , stringData = Some
      [ { mapKey = Constants.OciRegistrySecret.name
        , mapValue = Constants.OciRegistrySecret.value
        }
      ]
    , immutable = Some True
    , type = Some "Opaque"
    }
