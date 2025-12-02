let Constants = ../../types/constants.dhall

let CACertificate = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = Constants.mtls.caCertificateRequest, namespace = Constants.PolarNamespace }
, spec =
  { commonName = Constants.mtls.commonName
  , isCA = True
  , issuerRef = { kind = "Issuer", name = Constants.mtls.caCertificateIssuerName }
  , secretName = Constants.mtls.caCertName
  }
}

in CACertificate
