-- infra/layers/2-services/postgres/values.dhall
--
-- Canonical defaults for the postgres chart (backing store for Keycloak).
{ name             = "polar-postgres"
, image            = "docker.io/library/postgres:16"
, imagePullPolicy  = "IfNotPresent"
, storageClassName = Some "managed-csi"
, storageSize      = "5Gi"
, dbName           = "keycloak"
, dbUser           = "keycloak"
}
