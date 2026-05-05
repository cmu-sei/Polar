-- Schema for the cert issuer's ServiceConfig.
--
-- This file is the single source of truth for the config types.
-- Operators write per-environment configs that import this schema,
-- override the fields that vary per environment, and use the
-- defaults for everything else.
--
-- The schema mirrors the Rust ServiceConfig struct in
-- cert-issuer/src/config.rs. If you change the struct, change this
-- file.

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
          { url : Text
          , provisioner : Text
          , provisioner_key_path : Text
          , default_lifetime : Duration
          }
      , default =
          { url = ""
          , provisioner = ""
          , -- The path inside the running container where the
            -- provisioner private key is mounted. In production
            -- this is a Kubernetes Secret mounted as a file; in
            -- development it's wherever you generated the key.
            provisioner_key_path = "/etc/cert-issuer/provisioner.key"
          , -- 1 hour matches the v1 spec recommendation. The CA
            -- itself may clamp this further per its provisioner
            -- policy.
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
