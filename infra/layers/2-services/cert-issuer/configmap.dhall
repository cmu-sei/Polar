-- infra/layers/2-services/cert-issuer/configmap.dhall
--
-- Renders the cert-issuer's JSON config into a ConfigMap.
-- The binary reads this at startup via CERT_ISSUER_CONFIG.
let Constants =
      ../../../schema/constants.dhall

let JSON =
      https://prelude.dhall-lang.org/JSON/package.dhall
        sha256:5f98b7722fd13509ef448b075e02b9ff98312ae7a406cf53ed25012dbc9990ac

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

-- Mirrors the Rust IdentityNormalization enum
-- (#[serde(tag = "strategy", rename_all = "snake_case")]) exactly.
-- A real Dhall union rather than a Text discriminator, since Dhall has
-- no Text equality operator at all -- `merge` is the native mechanism
-- for this kind of branching, and it's type-checked: a
-- KubernetesServiceAccount variant simply cannot be constructed
-- without its clusterDomain, no runtime "is this set" check needed.
let IdentityStrategy = ../../../schema/identity-strategy.dhall

let CaProvisioning = ../../../schema/ca-provisioning.dhall

let IssuerCfg =
      { issuerUrl : Text
      , audience : List Text
      , jwksUri : Optional Text
      , workloadIdentityClaim : Text
      , instanceBindingClaim : Text
      , identityStrategy : IdentityStrategy
      -- Only meaningful when identityStrategy is SuffixedClaim. Names
      -- a second claim in the same token supplying the
      -- identity-scoping value.
      , identityScopeClaim : Optional Text
      , allowedAlgorithms : List Text
      , jwksCacheTtlMinSecs : Natural
      , jwksCacheTtlMaxSecs : Natural
      }

let render =
      \ ( v
        : { name : Text
          , port : Natural
          , caCertPath : Text
          , caKeyPath : Text
          , issuers : List IssuerCfg
          , defaultLifetimeSecs : Natural
          , serverLifetimeSecs : Natural
          , userLifetimeSecs : Natural
          , identityLifetimeOverrides : List { identity : Text, lifetimeSecs : Natural }
          -- DNS identity cert-issuer mints into its own TLS server
          -- certificate at startup. Must be the name callers actually
          -- connect to. A config value, not hardcoded, so the cluster
          -- domain stays configurable per deployment.
          , mtlsServerIdentity : Text
          -- Identities allowed to reach /internal/issue, matched
          -- against the connection's verified mTLS peer SAN. Empty
          -- means the endpoint is unreachable, which is the safe default.
          , mtlsInternalCallers : List Text
          -- Where cert-issuer's CA comes from. See ca-provisioning.dhall.
          , caProvisioning : CaProvisioning
          }
        ) ->
        let renderIssuer =
              \(iss : IssuerCfg) ->
                let jwks =
                      merge
                        { Some = \(uri : Text) -> JSON.string uri, None = JSON.null }
                        iss.jwksUri

                let identityNormalizationJson =
                      merge
                        { KubernetesServiceAccount =
                            \(cfg : { clusterDomain : Text }) ->
                              JSON.object
                                ( toMap
                                    { strategy =
                                        JSON.string "kubernetes_service_account"
                                    , cluster_domain = JSON.string cfg.clusterDomain
                                    }
                                )
                        , SuffixedClaim =
                            JSON.object
                              (toMap { strategy = JSON.string "suffixed_claim" })
                        }
                        iss.identityStrategy

                let identityScopeClaimJson =
                      merge
                        { Some = \(c : Text) -> JSON.string c, None = JSON.null }
                        iss.identityScopeClaim

                in  JSON.object
                      ( toMap
                          { issuer = JSON.string iss.issuerUrl
                          , audience =
                              JSON.array
                                ( Prelude.List.map
                                    Text
                                    JSON.Type
                                    JSON.string
                                    iss.audience
                                )
                          , jwks_uri = jwks
                          , workload_identity_claim = JSON.string iss.workloadIdentityClaim
                          , instance_binding_claim = JSON.string iss.instanceBindingClaim
                          , identity_normalization = identityNormalizationJson
                          , identity_scope_claim = identityScopeClaimJson
                          , allowed_algorithms =
                              JSON.array
                                ( Prelude.List.map
                                    Text
                                    JSON.Type
                                    JSON.string
                                    iss.allowedAlgorithms
                                )
                          , jwks_cache_ttl_min =
                              JSON.object
                                ( toMap
                                    { secs = JSON.natural iss.jwksCacheTtlMinSecs
                                    , nanos = JSON.natural 0
                                    }
                                )
                          , jwks_cache_ttl_max =
                              JSON.object
                                ( toMap
                                    { secs = JSON.natural iss.jwksCacheTtlMaxSecs
                                    , nanos = JSON.natural 0
                                    }
                                )
                          }
                      )

        let configJson =
              JSON.render
                ( JSON.object
                    ( toMap
                        { bind_addr =
                            JSON.string "0.0.0.0:${Natural/show v.port}"
                        , ca =
                            JSON.object
                              ( toMap
                                  { ca_cert_path = JSON.string v.caCertPath
                                  , ca_key_path = JSON.string v.caKeyPath
                                  , provisioning =
                                      merge
                                        { Provisioned = JSON.string "provisioned"
                                        , SelfBootstrap = JSON.string "self_bootstrap"
                                        }
                                        v.caProvisioning
                                  , default_lifetime =
                                      JSON.object
                                        ( toMap
                                            { secs = JSON.natural v.defaultLifetimeSecs
                                            , nanos = JSON.natural 0
                                            }
                                        )
                                  , server_lifetime =
                                      JSON.object
                                        ( toMap
                                            { secs = JSON.natural v.serverLifetimeSecs
                                            , nanos = JSON.natural 0
                                            }
                                        )
                                  , user_lifetime =
                                      JSON.object
                                        ( toMap
                                            { secs = JSON.natural v.userLifetimeSecs
                                            , nanos = JSON.natural 0
                                            }
                                        )
                                  , identity_lifetime_overrides =
                                      JSON.object
                                        ( Prelude.List.map
                                            { identity : Text, lifetimeSecs : Natural }
                                            { mapKey : Text, mapValue : JSON.Type }
                                            ( \(o : { identity : Text, lifetimeSecs : Natural }) ->
                                                { mapKey = o.identity
                                                , mapValue =
                                                    JSON.object
                                                      ( toMap
                                                          { secs = JSON.natural o.lifetimeSecs
                                                          , nanos = JSON.natural 0
                                                          }
                                                      )
                                                }
                                            )
                                            v.identityLifetimeOverrides
                                        )
                                  }
                              )
                        , issuers =
                            JSON.array
                              ( Prelude.List.map
                                  IssuerCfg
                                  JSON.Type
                                  renderIssuer
                                  v.issuers
                              )
                        , mtls =
                            JSON.object
                              ( toMap
                                  { server_identity =
                                      JSON.string v.mtlsServerIdentity
                                  , internal_callers =
                                      JSON.array
                                        ( Prelude.List.map
                                            Text
                                            JSON.Type
                                            JSON.string
                                            v.mtlsInternalCallers
                                        )
                                  }
                              )
                        }
                    )
                )

        in  { apiVersion = "v1"
            , kind = "ConfigMap"
            , metadata =
              { name = "${v.name}-config"
              , namespace = Constants.PolarNamespace
              }
            , data.`cert-issuer.json` = configJson
            }

in  render
