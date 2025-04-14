let values = ../values.dhall

let CertificateIssuer = { apiVersion = "cert-manager.io/v1"
, kind = "Issuer"
, metadata.name = values.neo4j.tls.leafIssuer
, metadata.namespace = values.neo4j.namespace
, spec.ca = { secretName = values.neo4j.tls.caSecretName } 
}

in CertificateIssuer
