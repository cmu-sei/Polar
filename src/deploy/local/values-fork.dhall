{-
  local/values-fork.dhall — values for the fork's platform on a local cluster.

  The platform the Blitz agents (and any other builder agent) run on: the certificate issuer, Cassini,
  the credentials agent and its observer, and a graph. Everything that differs between clusters is
  here; the expressions in fork/ contain no address, tag or name of their own. No secret is here: the
  expressions reference them by name (see fork/README.md).

  The images are the fork's own, loaded into the node by hand, so they are not pulled.
-}
let Polar = ../types/package.dhall

let C = ../types/lib-constants.dhall

let B = Polar.builder

let namespace = C.polarNamespace

let certIssuerUrl = "https://cert-issuer.${namespace}.svc.cluster.local:8443"

let observerUrl = "http://credentials-agent-observer.${namespace}.svc.cluster.local:8080"

in  { namespace
    , images =
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
    , certIssuerUrl
    , observerUrl
    -- The platform as an agent sees it. Ordinary agents are vouched for by the credentials agent;
    -- the platform's own pieces override that and go to the issuer directly (fork/30-agents).
    , platform =
        B.Platform::{
        , cassiniAddr = C.cassiniAddr
        , cassiniServerName = C.cassiniServerName
        , issuer = B.Issuer.CredentialsAgent observerUrl
        , polarInitImage = "polar-init:latest"
        , imagePullPolicy = Some "IfNotPresent"
        }
    , graph =
      { endpoint = "bolt://polar-db-svc.${namespace}.svc.cluster.local:7687"
      , database = "neo4j"
      , user = "neo4j"
      , passwordSecret = "polar-graph-pw"
      , passwordKey = "secret"
      }
    }
