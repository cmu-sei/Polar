{ apiVersion = "v1"
, kind = "Secret"
, metadata = { name = "neo4j-secret", namespace = "polar-db" }
, stringData.NEO4J_AUTH = "neo4j/changeit"
, type = "Opaque"
}
