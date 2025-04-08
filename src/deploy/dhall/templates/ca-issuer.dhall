let values = ../values.dhall

let CertificateIssuer = { apiVersion = "cert-manager.io/v1"
, kind = "ClusterIssuer"
, metadata.name = values.mtls.caCertificateIssuerName
, spec.selfSigned = {=}
}

in CertificateIssuer
