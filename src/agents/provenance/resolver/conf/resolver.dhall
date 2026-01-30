-- simple data types to configure the resolver with.
let Registry =
      { name : Text
      , url : Text
      , clientCertPath : Optional Text
      }

let ResolverConfig =
      { registries : List Registry }

let config : ResolverConfig =
    { registries =
        [ { name = "sandbox-acr"
        , url = "sanbboxaksacr.azurecr.us"
            , clientCertPath = None Text
            }
        ]
    }

in config
