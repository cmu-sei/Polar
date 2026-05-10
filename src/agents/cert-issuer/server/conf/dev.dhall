
-- Example cert issuer config for a local development cluster.
--
-- Render to JSON for the binary to consume:
--   dhall-to-json --file dev.dhall > dev.json
--   CERT_ISSUER_CONFIG=dev.json ./target/debug/cert-issuer
--


let schema = ./schema.dhall

let cluster_name = "dev"

in  schema.ServiceConfig::{
    , bind_addr = "127.0.0.1:8443"
    , issuer = schema.IssuerConfig::{
      , -- The Kubernetes API server's OIDC issuer URL. For
        -- in-cluster clients this is normally
        -- https://kubernetes.default.svc; for local development
        -- you'll typically run a small OIDC stub instead.
        issuer = "http://localhost:8080"
      , -- Tokens must request this audience via the projected SA
        -- token's `audiences` field in the pod spec (or via your
        -- OIDC stub's claims). Every pod that wants to attest must
        -- claim the same value.
        audience = "polar-cert-issuer.${cluster_name}"
      , jwks_uri = Some "http://localhost:8080/jwks.json"
      }
    , ca = schema.CaConfig::{
      , ca_cert_path = "./tmp/ca.crt"
      , ca_key_path = "./tmp/ca.key"
      , -- Slightly shorter lifetime in dev to surface
        -- restart-driven renewal issues quickly. Production
        -- typically uses the 1-hour default.
        default_lifetime = schema.minutes 30
      }
    }
