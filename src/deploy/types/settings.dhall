{-
  types/settings.dhall — environment variables as the library's agents are given them, written once.
-}
let kubernetes = ./kubernetes.dhall

--| A variable with a value.
let setting =
      λ(name : Text) →
      λ(value : Text) →
        kubernetes.EnvVar::{ name, value = Some value }

--| A variable whose value is a key of a Secret.
let secretSetting =
      λ(name : Text) →
      λ(secretName : Text) →
      λ(key : Text) →
        kubernetes.EnvVar::{
        , name
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some kubernetes.SecretKeySelector::{ name = Some secretName, key }
          }
        }

--| The graph, as an agent that writes to it reads its settings. No certificate settings: the
--| development graph has no TLS.
let graphSettings =
      λ(namespace : Text) →
        [ setting "GRAPH_ENDPOINT" "bolt://polar-db-svc.${namespace}.svc.cluster.local:7687"
        , setting "GRAPH_DB" "neo4j"
        , setting "GRAPH_USER" "neo4j"
        , secretSetting "GRAPH_PASSWORD" "polar-graph-pw" "secret"
        ]

in  { setting, secretSetting, graphSettings }
