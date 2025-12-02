let Constants = ../../types/constants.dhall

let CertificateIssuer = { apiVersion = "cert-manager.io/v1"
, kind = "Issuer"
, metadata = {
    name = Constants.mtls.caCertificateIssuerName
    , namespace = Constants.PolarNamespace

}
, spec.selfSigned = {=}
}

in CertificateIssuer
