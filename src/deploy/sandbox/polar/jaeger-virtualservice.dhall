let values = ../values.dhall

let VirtualService = { apiVersion = "networking.istio.io/v1"
, kind = "VirtualService"
, metadata =
  {
  , name = "jaeger-vs"
  , namespace = values.neo4j.namespace
  }
, spec =
  { gateways = [ "istio-system/public" ]
  , hosts = [ "jaeger.${values.sandboxHostSuffix}" ]
  , http =
    [
     { match = [ { uri.prefix = "/" } ]
      , route =
        [ { destination =
            { host = values.jaegerDNSName
            , port.number = values.jaeger.ports.http
            }
          }
        ]
      }
    ]
  }
}

in VirtualService
