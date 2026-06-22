
-- Schema for the cert issuer's ServiceConfig.
--
-- This file is the single source of truth for the config types.
-- Operators write per-environment configs that import this schema,
-- override the fields that vary per environment, and use the
-- defaults for everything else.
--
-- The schema mirrors the Rust ServiceConfig struct in
-- cert-issuer/src/config.rs. If you change the struct, change this
-- file. The CI pipeline runs `dhall-to-json` on each environment's
-- config and feeds the result to the Rust validator at startup, so
-- a schema-vs-code drift surfaces as a CI failure rather than a
-- runtime crash.

let Duration =
      -- Rust's serde-default Duration form is {secs: N, nanos: M}.
      -- We always emit nanos = 0; sub-second precision isn't
      -- meaningful for any cert issuer timeout. If we ever switch
      -- the Rust config to humantime-style strings ("1h", "30s"),
      -- this type alias becomes `Text` and the helpers below go
      -- away.
      { secs : Natural, nanos : Natural }

let seconds = \(n : Natural) -> { secs = n, nanos = 0 }

let minutes = \(n : Natural) -> seconds (n * 60)

let hours = \(n : Natural) -> seconds (n * 3600)

let IssuerConfig =
      { Type =
          { issuer : Text
          , audience : Text
          , jwks_uri : Optional Text
          , workload_identity_claim : Text
          , instance_binding_claim : Text
          , allowed_algorithms : List Text
          , jwks_cache_ttl_min : Duration
          , jwks_cache_ttl_max : Duration
          }
      , default =
          { -- Issuer and audience MUST be set per-environment;
            -- there are no sensible defaults. Listed here so the
            -- record-merge syntax doesn't reject configs that
            -- forget to set them — the Rust validator catches the
            -- empty strings at startup.
            issuer = ""
          , audience = ""
          , -- jwks_uri is optional in the Rust config (auto-discovery
            -- via OIDC well-known endpoint is intended for v2). For
            -- v1 you should always set this explicitly.
            jwks_uri = None Text
          , -- For Kubernetes projected SA tokens, the workload
            -- identity comes from the `sub` claim. SPIFFE/SPIRE
            -- environments would override this.
            workload_identity_claim = "sub"
          , -- JSON Pointer syntax: slash-separated, with the
            -- literal "kubernetes.io" preserved as one segment
            -- (it contains a dot). See the doc comment on
            -- `extract_string_claim` in oidc.rs for why this
            -- isn't dot-separated.
            instance_binding_claim = "kubernetes.io/pod/uid"
          , -- HS256/HS384/HS512 and "none" are rejected by the
            -- Rust config validator regardless of what's here, so
            -- there's no way to accidentally enable them through
            -- this default.
            allowed_algorithms = [ "RS256", "ES256", "EdDSA" ]
          , -- Cache TTL bounds. JWKS endpoints typically respond
            -- with Cache-Control: max-age set to something like
            -- 1 hour. The cert issuer clamps the server's value
            -- to these bounds.
            jwks_cache_ttl_min = seconds 30
          , jwks_cache_ttl_max = hours 1
          }
      }

let CaConfig =
      { Type =
          { ca_cert_path : Text
          , ca_key_path : Text
          , default_lifetime : Duration
          }
      , default =
          { -- Paths to the CA's self-signed cert and PKCS#8 PEM
            -- private key. The cert issuer holds these in memory
            -- for the service's lifetime and signs CSRs directly
            -- (no external CA process). Generate with:
            -- or with the `bootstrap_ca()` helper from `ca.rs`.
            ca_cert_path = "/home/polar/ca.crt"
          , ca_key_path = "/home/polar/ca.key"
          , -- 1 hour matches the v1 spec recommendation. Workload
            -- certs renew via init-container restart at expiry.
            default_lifetime = hours 1
          }
      }

let ServiceConfig =
      { Type =
          { bind_addr : Text
          , issuer : IssuerConfig.Type
          , ca : CaConfig.Type
          }
      , default =
          { bind_addr = "0.0.0.0:8443"
          , issuer = IssuerConfig::{=}
          , ca = CaConfig::{=}
          }
      }

in  { Duration
    , IssuerConfig
    , CaConfig
    , ServiceConfig
    , seconds
    , minutes
    , hours
    }
