-- infra/layers/2-services/cert-issuer/configmap.dhall
--
-- Renders the cert-issuer's JSON config into a ConfigMap.
-- The binary reads this at startup via CERT_ISSUER_CONFIG.
let Constants =
      ../../../schema/constants.dhall
        sha256:83837b5e87d39846c41a7ae549d5c65b2c2c8a058c6841b28ed1b746f4c976f2

let JSON =
      https://prelude.dhall-lang.org/JSON/package.dhall
        sha256:5f98b7722fd13509ef448b075e02b9ff98312ae7a406cf53ed25012dbc9990ac

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let render =
      \ ( v
        : { name : Text
          , port : Natural
          , caCertPath : Text
          , caKeyPath : Text
          , oidcIssuerUrl : Text
          , oidcAudience : List Text
          , oidcJwksUri : Optional Text
          }
        ) ->
        let jwks =
              merge
                { Some = \(uri : Text) -> JSON.string uri, None = JSON.null }
                v.oidcJwksUri

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
                                  , default_lifetime =
                                      JSON.object
                                        ( toMap
                                            { secs = JSON.natural 3600
                                            , nanos = JSON.natural 0
                                            }
                                        )
                                  }
                              )
                        , issuer =
                            JSON.object
                              ( toMap
                                  { issuer = JSON.string v.oidcIssuerUrl
                                  , audience =
                                      JSON.array
                                        ( Prelude.List.map
                                            Text
                                            JSON.Type
                                            JSON.string
                                            v.oidcAudience
                                        )
                                  , jwks_uri = jwks
                                  , workload_identity_claim = JSON.string "sub"
                                  , instance_binding_claim =
                                      JSON.string "kubernetes.io/pod/uid"
                                  , allowed_algorithms =
                                      JSON.array
                                        [ JSON.string "RS256"
                                        , JSON.string "ES256"
                                        , JSON.string "EdDSA"
                                        ]
                                  , jwks_cache_ttl_min =
                                      JSON.object
                                        ( toMap
                                            { secs = JSON.natural 30
                                            , nanos = JSON.natural 0
                                            }
                                        )
                                  , jwks_cache_ttl_max =
                                      JSON.object
                                        ( toMap
                                            { secs = JSON.natural 3600
                                            , nanos = JSON.natural 0
                                            }
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
