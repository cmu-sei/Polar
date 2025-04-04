
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let configContent = ../../../../conf/neo4j_setup/conf/neo4j.conf as Text  -- Reads the file as raw text

let neo4jConfig = kubernetes.ConfigMap::{ 
    apiVersion = "v1"
    , kind = "ConfigMap"
    , metadata = kubernetes.ObjectMeta::{ name = Some values.neo4j.config.name, namespace = Some values.namespace }
    , data = Some [ { mapKey = "neo4j.conf", mapValue = configContent  } ]
}

in  neo4jConfig
