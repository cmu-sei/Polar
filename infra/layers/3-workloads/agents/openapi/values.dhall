-- infra/layers/3-workloads/agents/openapi/values.dhall
--
-- Canonical defaults for the openapi agent chart.
-- Observer fetches OpenAPI specs and publishes to Cassini.
-- Processor reads Cassini topics and writes to the graph.

let Constants = ../../../../schema/constants.dhall

in  { name            = "openapi-agents"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , observer =
      { name            = "openapi-observer"
      , image           = "openapi-observer:latest"
      , openapiEndpoint = "http://localhost:3000/api-docs/openapi.json"
      }

    , processor =
      { name  = "openapi-processor"
      , image = "openapi-processor:latest"
      }

    , tls =
      { certificateRequestName = "openapi-agent-certificate"
      , certificateSpec =
        { commonName  = Constants.mtls.commonName
        , dnsNames    = [ Constants.cassiniDNSName ]
        , duration    = "2160h"
        , issuerRef   = { kind = "Issuer", name = Constants.mtls.leafIssuerName }
        , renewBefore = "360h"
        , secretName  = "openapi-agent-tls"
        }
      }
    }
