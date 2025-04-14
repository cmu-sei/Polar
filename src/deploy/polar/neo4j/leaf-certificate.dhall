let values = ../values.dhall

let CertificateRequest = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = "neo4j-certificate", namespace = values.neo4j.namespace }
, spec =
  { dnsNames = [ values.neo4jDNSName ]
  -- Apparently, we're not exactly supposed to do this, but Neo4j demands a DN field be set
  -- and this is the easiest workaround.
  -- REFERENCE: https://github.com/cert-manager/cert-manager/issues/3634
  -- REFERENCE: https://github.com/kubernetes-client/java/issues/1926
  , commonName = "PolarDB"
  , issuerRef = { kind = "Issuer", name = values.neo4j.tls.leafIssuer }
  , secretName = values.neo4j.tls.leafSecretName
  , usages = [ "server auth" ]
  }
}

in CertificateRequest