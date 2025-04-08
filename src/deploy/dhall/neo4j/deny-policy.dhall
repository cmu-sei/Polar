let values = ../values.dhall
-- Istio evaluates DENY before ALLOW,
-- so this works like a default-drop firewall rule.
let DenyPolicy = { apiVersion = "security.istio.io/v1beta1"
, kind = "AuthorizationPolicy"
, metadata = { name = "deny-all-neo4j", namespace = values.neo4j.namespace }
, spec = { action = "DENY", selector.matchLabels.app = values.neo4j.name }
}

in DenyPolicy