{ apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = "gitlab-agent-certificate", namespace = "polar" }
, spec =
  { commonName = "polar"
  , dnsNames = [ "cassini-ip-svc.polar.svc.cluster.local" ]
  , duration = "2160h"
  , issuerRef = { kind = "Issuer", name = "polar-leaf-issuer" }
  , renewBefore = "360h"
  , secretName = "client-tls"
  }
}
