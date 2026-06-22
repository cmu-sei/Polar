-- infra/layers/2-services/neo4j/script-configmap.dhall
--
-- ConfigMap containing the neo4j init script.
-- Mounted at /scripts/init.nu in the polar-nu-init init container.
-- Imports setup-neo4j.nu directly as Text — no string escaping needed.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let scriptContent = ./setup-neo4j.nu as Text

in  kubernetes.ConfigMap::{
    , apiVersion = "v1"
    , kind       = "ConfigMap"
    , metadata   = kubernetes.ObjectMeta::{
      , name      = Some "neo4j-init-script"
      , namespace = Some Constants.GraphNamespace
      }
    , data = Some
      [ { mapKey = "init.nu", mapValue = scriptContent } ]
    }
