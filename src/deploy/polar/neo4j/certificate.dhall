let values = ../values.dhall

let CertificateRequest = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = "neo4j-certificate", namespace = values.neo4j.namespace }
, spec =
  { dnsNames = [ values.neo4jDNSName ]
  , issuerRef = { kind = "Issuer", name = values.neo4j.tls.certificateIssuer }
  , secretName = values.neo4j.tls.secretName
  , usages = [ "server auth" ]
  }
}

in CertificateRequest