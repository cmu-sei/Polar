let values = ../values.dhall

let CACertificate = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = values.mtls.caCertName, namespace = values.namespace }
, spec =
  { commonName = values.mtls.commonName
  , isCA = True
  , issuerRef = { kind = "ClusterIssuer", name = values.mtls.caCertificateIssuerName }
  , secretName = values.mtls.caCertName
  }
}

in CACertificate