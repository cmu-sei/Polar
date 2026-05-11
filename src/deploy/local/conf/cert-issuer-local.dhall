
{-
  conf/cert-issuer-local.dhall

  Cert issuer config for local cluster development via OrbStack or usernetes.
  The OIDC issuer is the local API server. The CA materials are bootstrapped
  by the cert issuer on first run and persisted to the configured PVC mount.

  Render to JSON before deploying:
    dhall-to-json --file conf/cert-issuer-local.dhall > conf/cert-issuer.json
-}


let CertIssuer = ../../../agents/cert-issuer/server/conf/cert-issuer.dhall

in  CertIssuer.ServiceConfig::{
    , bind_addr = "0.0.0.0:8443"
    , issuer = CertIssuer.IssuerConfig::{
        , issuer   = "https://kubernetes.default.svc.cluster.local"
        , audience = "polar-cert-issuer.local"
        , jwks_uri = Some "https://kubernetes.default.svc.cluster.local/openid/v1/jwks"
        }
    , ca = CertIssuer.CaConfig::{
        , ca_cert_path   = "/home/polar/ca/ca.crt"
        , ca_key_path    = "/home/polar/ca/ca.key"
        , default_lifetime = CertIssuer.minutes 30
        }
    }
