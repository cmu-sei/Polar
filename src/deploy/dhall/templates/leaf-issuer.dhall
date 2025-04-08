let values = ../values.dhall

let LeafIssuer = { apiVersion = "cert-manager.io/v1"
, kind = "ClusterIssuer"
, metadata.name = values.mtls.leafIssuerName
, spec.ca.secretName = values.mtls.caCertName
}

in LeafIssuer