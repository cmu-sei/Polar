-- infra/layers/2-services/neo4j/configmap.dhall
--
-- Neo4j ConfigMap containing neo4j.conf.
-- The conf file content is read from targets/<target>/conf/ by render.nu
-- and passed in as configContent.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v :
          { namespace    : Text
          , configContent : Text
          }
      ) ->
        kubernetes.ConfigMap::{
        , apiVersion = "v1"
        , kind       = "ConfigMap"
        , metadata   = kubernetes.ObjectMeta::{
          , name      = Some Constants.neo4jConfigmapName
          , namespace = Some v.namespace
          }
        , data = Some
          [ { mapKey = "neo4j.conf", mapValue = v.configContent } ]
        }

in render
