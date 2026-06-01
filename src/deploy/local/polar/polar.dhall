let kubernetes = ../../types/kubernetes.dhall

let Constants = ../../types/constants.dhall
let functions = ../../types/functions.dhall
let values = ../values.dhall

let namespace =
      kubernetes.Namespace::{
      , apiVersion = "v1"
      , kind = "Namespace"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.PolarNamespace
        }
      }

let neo4jCredentialSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "polar-graph-pw"
        , namespace = Some Constants.PolarNamespace
        }
      , stringData = Some
        [ { mapKey = Constants.neo4jSecret.key
          , mapValue = env:GRAPH_PASSWORD as Text
          }
        ]
      , type = Some "Opaque"
      }

let ociSecret = env:DOCKER_AUTH_JSON as Text

-- =============================================================================
-- Init script ConfigMap — emitted once, mounted into every pod that leverages mTLS
-- =============================================================================

let agentInitScriptConfigMap =
    functions.makeNuInitScript
        Constants.initScriptConfigMapName
        Constants.polarInitScript

in  [ kubernetes.Resource.Namespace namespace
    , kubernetes.Resource.Secret neo4jCredentialSecret
    , kubernetes.Resource.ConfigMap agentInitScriptConfigMap
    ]
