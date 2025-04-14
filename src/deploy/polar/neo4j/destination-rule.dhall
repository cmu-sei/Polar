let values = ../values.dhall
let Rule = {
    apiVersion = "networking.istio.io/v1"
    , kind = "DestinationRule"
    ,metadata = {
        name = "neo4j-simple"
        , namespace = values.neo4j.namespace
    }
    ,spec = {
        host = values.neo4jDNSName
        , trafficPolicy = {
            tls.mode = "SIMPLE"
        }
        , credentialName = values.neo4j.tls.leafSecretName
    }
}

in Rule