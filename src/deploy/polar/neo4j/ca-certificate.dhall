let values = ../values.dhall

let CACertificate = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = "self-signed-root", namespace = values.neo4j.namespace }
, spec =
  { commonName = "PolarDB Root CA"
  , isCA = True
  , issuerRef = { kind = "Issuer", name = values.neo4j.tls.caIssuerName }
  , secretName = values.neo4j.tls.caSecretName
  }
}

in CACertificate