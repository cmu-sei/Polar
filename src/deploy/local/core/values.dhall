{-
  local/core/values.dhall — the core, on a local cluster.

  Everything that differs between clusters is here; the library's core (types/core.dhall) has no
  address, tag or name of its own. No secret is here: the core makes the ones it needs, with random
  values that are never printed (see plan.dhall's stages).

  The images are the fork's own, loaded into the node by hand, so they are not pulled.
-}
let Polar = ../../types/package.dhall

let values
    : Polar.core.CoreValues.Type
    = { images =
        { operator = "polar-operator:latest"
        , certIssuer = "cert-issuer:latest"
        , polarInit = "polar-init:latest"
        , cassini = "cassini:latest"
        , credentialsAgent = "credentials-agent-consumer:latest"
        , credentialsAgentObserver = "credentials-agent-observer:latest"
        , neo4j = "neo4j:5.26.2"
        }
      , imagePullPolicy = Some "IfNotPresent"
      , storageClass = None Text
      }

in  values
