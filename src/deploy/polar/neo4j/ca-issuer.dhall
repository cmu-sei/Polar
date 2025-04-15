let values = ../values.dhall

let CertificateIssuer = { apiVersion = "cert-manager.io/v1"
, kind = "ClusterIssuer"
, metadata = {
    name = values.neo4j.tls.caIssuerName
    , namespace = values.neo4j.namespace

}
, spec.selfSigned = {=}
}

in CertificateIssuer
