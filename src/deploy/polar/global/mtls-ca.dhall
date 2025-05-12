let values = ../values.dhall

let CACertificate = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = values.mtls.caCertificateRequest, namespace = values.namespace }
, spec =
  { commonName = values.mtls.commonName
  , isCA = True
  , issuerRef = { kind = "Issuer", name = values.mtls.caCertificateIssuerName }
  , secretName = values.mtls.caCertName
  }
}

in CACertificate