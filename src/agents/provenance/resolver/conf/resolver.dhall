-- simple data types to configure the resolver with.
let Registry =
      { name : Text
      , url : Text
      , clientCertPath : Optional Text
      }

let ResolverConfig =
      { registries : List Registry }

in {Registry, ResolverConfig}
