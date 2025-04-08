let values = ../values.dhall

let GraphGateWay = { apiVersion = "networking.istio.io/v1beta1"
, kind = "Gateway"
, metadata = { name = "graph-db-gateway"
  , namespace = values.neo4j.namespace }
, spec =
  {
  , servers =
    [ { hosts = [ values.neo4j.hostName ]
      , port = { name = "https", number = values.neo4jPorts.https, protocol = "HTTPS" }
      }
    ]
  }
}

in GraphGateWay