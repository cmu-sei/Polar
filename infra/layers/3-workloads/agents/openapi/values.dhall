-- infra/layers/3-workloads/agents/openapi/values.dhall
--
-- Canonical defaults for the openapi (web) agent chart.
-- Observer polls OpenAPI endpoints, consumer writes to graph.
--
-- STATUS: Work in progress. Not yet wired into any target.
-- This file is a placeholder — values will be filled in when
-- the agent is ready for deployment.

let Constants = ../../../../schema/constants.dhall

in  { name            = "openapi-agents"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , observer =
      { name  = "web-observer"
      , image = "polar-web-observer:latest"
      }

    , consumer =
      { name  = "web-consumer"
      , image = "polar-web-consumer:latest"
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

    , proxyCACert = None Text
    }
