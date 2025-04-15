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
            tls = {
                mode = "SIMPLE"
                -- Istio's gateway needs to have access to our CA cert, unfortunately we don't really control it
                --  TODO: In the future, something like the following should be added.
                -- caCertificates = /etc/tls/neo-ca.crt
                , insecureSkipVerify = True
                
            }
        }
    }
}

in Rule