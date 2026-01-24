let Resolver = ./conf/resolver.dhall

let config : Resolver.ResolverConfig =
      { registries =
          [ { name = "sandbox-acr"
        , url = "sanbboxaksacr.azurecr.us"
            , clientCertPath = None Text
            }
          ]
      }

in config
