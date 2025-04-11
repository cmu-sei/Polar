let values = ../values.dhall
-- Istio evaluates DENY before ALLOW,
-- so this works like a default-drop firewall rule.
let DenyPolicy = { apiVersion = "security.istio.io/v1"
, kind = "AuthorizationPolicy"
, metadata = { name = "deny-all-neo4j", namespace = values.neo4j.namespace }
--  Deny every request not covered by exclusions in the allow policy
, spec = { action = "DENY", selector.matchLabels.app = values.neo4j.name, rules = [ {=} ] }
}

in DenyPolicy