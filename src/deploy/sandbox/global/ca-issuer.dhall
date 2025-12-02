let values = ../values.dhall

let CertificateIssuer = { apiVersion = "cert-manager.io/v1"
, kind = "Issuer"
, metadata = {
    name = values.mtls.caCertificateIssuerName
    , namespace = values.namespace

}
, spec.selfSigned = {=}
}

in CertificateIssuer
