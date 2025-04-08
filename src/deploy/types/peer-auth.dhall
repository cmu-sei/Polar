let values = ../values.dhall

let PeerAuth = { apiVersion = "security.istio.io/v1"
, kind = "PeerAuthentication"
, metadata = { name = "allow-polar-svcs", namespace = values.neo4j.namespace }
, spec = { mtls.mode = "PERMISSIVE", selector.matchLabels.app = values.neo4j.name }
}

in PeerAuth
