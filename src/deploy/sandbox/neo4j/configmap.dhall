
let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

let configContent = ../../../conf/neo4j.conf as Text  -- Reads the file as raw text

let neo4jConfig = kubernetes.ConfigMap::{ 
    apiVersion = "v1"
    , kind = "ConfigMap"
    , metadata = kubernetes.ObjectMeta::{ name = Some values.neo4j.config.name, namespace = Some values.neo4j.namespace }
    , data = Some [ { mapKey = "neo4j.conf", mapValue = configContent  } ]
}

in  neo4jConfig
