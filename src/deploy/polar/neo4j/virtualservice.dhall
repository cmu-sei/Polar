let values = ../values.dhall

let VirtualService = { apiVersion = "networking.istio.io/v1"
, kind = "VirtualService"
, metadata =
  {
  , name = "polar-db-vs"
  , namespace = values.neo4j.namespace
  }
, spec =
  { gateways = [ "istio-system/public" ]
  , hosts = [ "graph-db.${values.sandboxHostSuffix}" ]
  , http =
    [ 
     { match = [ { uri.prefix = "/" } ]
      -- , rewrite = None { uri : Text }
      , route =
        [ { destination =
            { host = values.neo4jDNSName
            , port.number = values.neo4jPorts.https
            }
          }
        ]
      }
    ]
  }
}

in VirtualService
