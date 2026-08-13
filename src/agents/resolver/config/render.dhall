--| Lowering from the `schema.dhall` types to the flat record that becomes YAML.
--
--  `dhall-to-yaml` erases union tags: `Protocol.HttpsExcept ["a"]` serialises to
--  the bare list `["a"]`, indistinguishable from any other list. So every sum
--  type in the schema is lowered here into an explicit `mode`-style
--  discriminator plus its payload fields. The Rust loader reads the flat form
--  and reconstructs the sum type immediately on parse, so nothing downstream of
--  `ResolverConfig::try_from` ever sees a representable-but-invalid state.
--
--  Usage:
--    dhall-to-yaml --preserve-null --file ./to-yaml.dhall > resolver.yaml

let S = ./schema.dhall

let TlsOut =
      { mode : Text
      , plaintextHosts : List Text
      , extraRootCertificates : List Text
      , exclusiveTrustStore : Bool
      }

let RegistryOut =
      { unqualifiedRefs : Text
      , defaultRegistry : Optional Text
      , defaultNamespace : Optional Text
      }

let renderProtocol
    : S.Protocol -> { mode : Text, plaintextHosts : List Text }
    = \(p : S.Protocol) ->
        merge
          { Https = { mode = "https", plaintextHosts = [] : List Text }
          , HttpsExcept =
              \(hosts : List Text) ->
                { mode = "https-except", plaintextHosts = hosts }
          , HttpInsecure =
              { mode = "http-insecure", plaintextHosts = [] : List Text }
          }
          p

let renderUnqualified
    : S.UnqualifiedRefPolicy -> RegistryOut
    = \(u : S.UnqualifiedRefPolicy) ->
        merge
          { Skip =
              { unqualifiedRefs = "skip"
              , defaultRegistry = None Text
              , defaultNamespace = None Text
              }
          , Qualify =
              \(d : S.DefaultRegistry) ->
                { unqualifiedRefs = "qualify"
                , defaultRegistry = Some d.registry
                , defaultNamespace = d.namespace
                }
          }
          u

let renderTls
    : S.Tls -> TlsOut
    = \(t : S.Tls) ->
          renderProtocol t.protocol
        /\  { extraRootCertificates = t.extraRootCertificates
            , exclusiveTrustStore = t.exclusiveTrustStore
            }

let render
    : S.Config.Type ->
        { registry : RegistryOut
        , tls : TlsOut
        , auth : S.Auth
        , http : S.Http
        , mirrors : List S.Mirror
        }
    = \(c : S.Config.Type) ->
        { registry = renderUnqualified c.unqualifiedRefs
        , tls = renderTls c.tls
        , auth = c.auth
        , http = c.http
        , mirrors = c.mirrors
        }

in  render
