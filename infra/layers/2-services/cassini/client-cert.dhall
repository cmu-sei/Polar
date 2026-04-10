-- infra/layers/2-services/cassini/client-cert.dhall
--
-- Cassini client TLS certificate issued to agents.
-- All values derive from constants — no target overrides needed.

let Constants = ../../../schema/constants.dhall

in  { apiVersion = "cert-manager.io/v1"
    , kind       = "Certificate"
    , metadata   =
      { name      = "cassini-client-certificate"
      , namespace = Constants.PolarNamespace
      }
    , spec =
      { commonName  = Constants.mtls.commonName
      , dnsNames    = [ "cassini-ip-svc.polar.svc.cluster.local" ]
      , duration    = "2160h"
      , issuerRef   = { kind = "Issuer", name = Constants.mtls.leafIssuerName }
      , renewBefore = "360h"
      , secretName  = "client-tls"
      }
    }
