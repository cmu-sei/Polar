let values = ../values.dhall

let CertificateIssuer = { apiVersion = "cert-manager.io/v1"
, kind = "Issuer"
, metadata.name = values.neo4j.tls.certificateIssuer
, metadata.namespace = values.neo4j.namespace
, spec.selfSigned = {=}
}

in CertificateIssuer
