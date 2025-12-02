let values = ../values.dhall

-- Defines a seperate virtual service to handle websocket traffic and direct it to neo4j's bolt port

let WebsocketService = { apiVersion = "networking.istio.io/v1"
, kind = "VirtualService"
, metadata = { name = "polar-db-ws-svc", namespace = values.neo4j.namespace }
, spec =
  { gateways = [ "istio-system/public" ]
  , hosts = [ "graph-db-ws.${values.sandboxHostSuffix}" ]
  , http =
    [ { match = [ { uri.prefix = "/" } ]
      , route =
        [ { destination =
            { host = "polar-db-svc.polar-db.svc.cluster.local"
            , port.number = values.neo4jPorts.bolt
            }
          }
        ]
      }
    ]
  }
}

in WebsocketService