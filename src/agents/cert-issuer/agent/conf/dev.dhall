-- Example cert issuer config for a local development cluster.
--
-- Render to JSON for the binary to consume:
--   dhall-to-json --file dev.dhall > dev.json
--   CERT_ISSUER_CONFIG=dev.json ./target/debug/cert-issuer
--
-- For production environments, copy this file, rename it (e.g.
-- prod.dhall), and override the fields that vary. The schema
-- defaults handle everything else.

let schema = ./schema.dhall

let cluster_name = "dev"

in  schema.ServiceConfig::{
    , bind_addr = "0.0.0.0:8443"
    , issuer = schema.IssuerConfig::{
      , -- The Kubernetes API server's OIDC issuer URL. For
        -- in-cluster clients this is normally https://kubernetes.default.svc;
        -- for tools running on the host with kubectl-style access,
        -- it's the cluster's external API URL.
        issuer = "https://kubernetes.default.svc"
      , -- Tokens must request this audience via the projected SA
        -- token's `audiences` field in the pod spec. The string
        -- itself is arbitrary but every pod that wants to attest
        -- must claim the same value.
        audience = "polar-cert-issuer.${cluster_name}"
      , jwks_uri = Some
          "https://kubernetes.default.svc/openid/v1/jwks"
      }
    , ca = schema.CaConfig::{
      , url = "https://step-ca.polar-system.svc.cluster.local:9000"
      , provisioner = "cert-issuer"
      , provisioner_key_path = "/home/vcaaron/projects/Polar/src/agents/cert-issuer/agent/conf/provisioner.key"
      , -- Slightly shorter lifetime in dev to surface restart-driven
        -- renewal issues quickly. Production typically uses the
        -- 1-hour default.
        default_lifetime = schema.minutes 30
      }
    }
