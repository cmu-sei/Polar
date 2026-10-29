{-
  The platform's own agents, as PolarAgent resources: Cassini, the credentials agent and its observer.
  The operator keeps one instance of each alive.

  They differ from an ordinary agent in the ways the platform has to: they take their certificates
  from the issuer directly (the credentials agent is what vouches for everyone else, and Cassini is
  what the escrow bootstrap travels through), so none of them bootstraps an escrow key, and none waits
  for the broker (Cassini is the broker, and the other two must be able to start while it is down).

  Shutdown budgets are the least each agent's own `--scaffold` says it needs: 20, 50 and 40 seconds.
  Secrets are referenced: `cassini-shutdown-token` (key `token`) and `polar-graph-pw` (key `secret`).
-}
let Polar = ../../types/package.dhall

let C = ../../types/lib-constants.dhall

let kubernetes = ../../types/kubernetes.dhall

let values = ../values-fork.dhall

let B = Polar.builder

let setting =
      λ(name : Text) →
      λ(value : Text) →
        kubernetes.EnvVar::{ name, value = Some value }

let secret =
      λ(name : Text) →
      λ(secretName : Text) →
      λ(key : Text) →
        kubernetes.EnvVar::{
        , name
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some kubernetes.SecretKeySelector::{
            , name = Some secretName
            , key
            }
          }
        }

-- What every one of these has in common.
let platformAgent =
      B.BuilderAgent::{
      , name = ""
      , namespace = values.namespace
      , image = ""
      , issuer = Some (B.Issuer.CertIssuer values.certIssuerUrl)
      , waitForBroker = False
      , skipEscrow = True
      }

-- The broker. A server certificate for the name clients dial; it is not a client of itself.
let cassini =
        platformAgent
      // { name = "cassini"
        , image = values.images.cassini
        , provides = [] : List Text
        , shutdownBudgetSecs = 20
        , cert = B.CertKind.Server C.cassiniDNSName
        , client = False
        , port = Some C.cassiniTcpPort
        , settings =
          [ setting "TLS_SERVER_CERT_CHAIN" C.certPaths.cert
          , setting "TLS_SERVER_KEY" C.certPaths.key
          , setting "TLS_CA_CERT" C.certPaths.ca
          , setting "CASSINI_BIND_ADDR" "0.0.0.0:${Natural/show C.cassiniTcpPort}"
          , secret "CASSINI_SHUTDOWN_TOKEN" "cassini-shutdown-token" "token"
          ]
        }

-- Vouches for every other agent's certificate request, and keeps the ledger of what it issued.
let credentialsAgent =
        platformAgent
      // { name = "credentials-agent"
        , image = values.images.credentialsAgent
        , shutdownBudgetSecs = 50
        , data = Some
          { claim = "credentials-agent-data"
          , mountPath = "/var/lib/credentials-agent"
          }
        , settings =
          [ setting "CREDENTIALS_AGENT_CERT_ISSUER_URL" values.certIssuerUrl
          -- The graph is a projection of the ledger, and the agent runs without it; its settings are
          -- required all the same.
          , setting "GRAPH_ENDPOINT" values.graph.endpoint
          , setting "GRAPH_DB" values.graph.database
          , setting "GRAPH_USER" values.graph.user
          , secret "GRAPH_PASSWORD" values.graph.passwordSecret values.graph.passwordKey
          ]
        }

-- The HTTP front a pod without a certificate asks for one (`polar-init`).
let observer =
        platformAgent
      // { name = "credentials-agent-observer"
        , image = values.images.credentialsAgentObserver
        , shutdownBudgetSecs = 40
        , port = Some 8080
        }

in  [ B.makePolarAgent values.platform cassini
    , B.makePolarAgent values.platform credentialsAgent
    , B.makePolarAgent values.platform observer
    ]
