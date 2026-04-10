-- infra/layers/3-workloads/agents/build/values.dhall
--
-- Canonical defaults for the build agent chart.
-- Orchestrator: manages build jobs, needs cluster API access via SA token.
-- Processor: processes build results and writes to graph.
--
-- cyclops.yaml config is read from targets/<target>/conf/cyclops.yaml
-- by render.nu and injected as a Secret at render time.

let Constants = ../../../../schema/constants.dhall

in  { name            = "build-agents"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , orchestrator =
      { name               = "build-orchestrator"
      , image              = "build-orchestrator:latest"
      , serviceAccountName = "build-processor-sa"
      , secretName         = "build-processor-sa-token"
      }

    , processor =
      { name  = "build-processor"
      , image = "build-processor:latest"
      }

    , tls =
      { certificateRequestName = "build-agent-certificate"
      , certificateSpec =
        { commonName  = Constants.mtls.commonName
        , dnsNames    = [ Constants.cassiniDNSName ]
        , duration    = "2160h"
        , issuerRef   = { kind = "Issuer", name = Constants.mtls.leafIssuerName }
        , renewBefore = "360h"
        , secretName  = "build-agent-tls"
        }
      }

    , proxyCACert = None Text
    }
