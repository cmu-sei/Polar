-- infra/layers/2-services/keycloak/values.dhall
--
-- Canonical defaults for the keycloak chart.
-- Targets override only what differs via targets/<target>/overrides.dhall.
{ name            = "keycloak"
, image           = "quay.io/keycloak/keycloak:26.7.1"
, imagePullPolicy = "IfNotPresent"
, serviceType     = "NodePort"
, ports           = { http = 8080 }
, dbHost          = "polar-postgres"
, dbName          = "keycloak"
, dbUser          = "keycloak"
}
