let values = ../values.dhall

let GraphGateWay = { apiVersion = "networking.istio.io/v1"
, kind = "Gateway"
, metadata = { name = values.neo4j.gateway.name
  , namespace = values.neo4j.namespace }
, spec =
  {
  , servers =
    [ { hosts = [ values.neo4j.hostName ]
      , port = { name = "https", number = values.neo4jPorts.https, protocol = "HTTPS" }
      , tls = { mode = "SIMPLE", credentialName = values.neo4j.tls.leafSecretName }
      }
    ]
  }
}

in GraphGateWay