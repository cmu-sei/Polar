let values = ../values.dhall

-- This only exposes ports for HTTPS traffic, 
-- so external users can hit the HTTP API
-- but can’t reach Bolt (7687) because it’s not routed at all.
let VirtualService = { apiVersion = "networking.istio.io/v1"
, kind = "VirtualService"
, metadata = {
  name = values.neo4j.service.name
  , namespace = values.neo4j.namespace
}
, spec =
  -- Always use the istio public gateway for now
  { gateways = [ "istio-system/public" ]
  , hosts = [ values.neo4j.hostName ]
  , http =
    [ { match = [ { uri.prefix = "/" } ]
      , route =
        [ { destination =
            { host = values.neo4jDNSName, port.number = values.neo4jPorts.https }
          }
        ]
      }
    ]
  }
}

in VirtualService