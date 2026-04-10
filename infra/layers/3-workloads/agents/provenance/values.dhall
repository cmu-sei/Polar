-- infra/layers/3-workloads/agents/provenance/values.dhall
--
-- Canonical defaults for the provenance agent chart.
-- Linker: reads graph data and links provenance artifacts.
-- Resolver: resolves OCI registry references, needs docker config mounted.
--
-- Both deployments reject Istio sidecar injection unconditionally.

let Constants = ../../../../schema/constants.dhall

in  { imagePullPolicy  = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , linker =
      { name  = Constants.ProvenanceLinkerName
      , image = "provenance-linker-agent:latest"
      }

    , resolver =
      { name  = Constants.ProvenanceResolverName
      , image = "provenance-resolver-agent:latest"
      }

    , tls =
      { certificateRequestName = "provenance-agent-certificate"
      , certificateSpec =
        { commonName  = Constants.mtls.commonName
        , dnsNames    = [ Constants.cassiniDNSName ]
        , duration    = "2160h"
        , issuerRef   = { kind = "Issuer", name = Constants.mtls.leafIssuerName }
        , renewBefore = "360h"
        , secretName  = "provenance-agent-tls"
        }
      }

    , proxyCACert = None Text
    }
