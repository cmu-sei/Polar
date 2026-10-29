{-
  types/core.dhall — the core of a Polar deployment, as stages.

  The core is what any platform needs to run atop the graph or cooperate with its agents: the operator,
  the certificate issuer, Cassini, the credentials agent and its observer, and the graph. The operator is
  the one thing Kubernetes itself keeps alive (a one-replica Deployment); it keeps every PolarAgent alive,
  these included. There are no Jobs.

  An application plan includes the core, so one plan stands up an application with everything it needs:

    stages = Core.core values .stages # myApplicationStages

  and the application's own agents take `(Core.core values).platform`, which says where the broker and
  the credentials agent are, so that no application restates them. Several applications on one cluster
  include the same core; its stages are `shared`, so they are applied once and are not taken down with
  one application.

  The stages are in the order the core needs, each waiting on the one before: the operator and its CRD;
  the issuer, which every pod's init container asks for a certificate; Cassini; the credentials agent and
  its observer; the graph.
-}
let kubernetes = ./kubernetes.dhall

let C = ./lib-constants.dhall

let Builder = ./builder.dhall

let Platform = ./platform.dhall

let P = ./plan.dhall

--| What a deployer states about the core, once per environment.
let CoreValues =
      { Type =
          { images :
              { operator : Text
              , certIssuer : Text
              , polarInit : Text
              , cassini : Text
              , credentialsAgent : Text
              , credentialsAgentObserver : Text
              , neo4j : Text
              }
          , imagePullPolicy : Optional Text
          , storageClass : Optional Text
          }
      }

let ns = C.polarNamespace

let certIssuerUrl = "https://cert-issuer.${ns}.svc.cluster.local:8443"

let observerUrl = "http://credentials-agent-observer.${ns}.svc.cluster.local:8080"

--| The platform an application's agents are told about: where the broker is, and that certificates are
--| asked of the credentials agent, which vouches for them.
let platform
    : CoreValues.Type → Builder.Platform.Type
    = λ(v : CoreValues.Type) →
        Builder.Platform::{
        , cassiniAddr = C.cassiniAddr
        , cassiniServerName = C.cassiniServerName
        , issuer = Builder.Issuer.CredentialsAgent observerUrl
        , polarInitImage = v.images.polarInit
        , imagePullPolicy = v.imagePullPolicy
        }

let setting =
      λ(name : Text) →
      λ(value : Text) →
        kubernetes.EnvVar::{ name, value = Some value }

let secretSetting =
      λ(name : Text) →
      λ(secretName : Text) →
      λ(key : Text) →
        kubernetes.EnvVar::{
        , name
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some kubernetes.SecretKeySelector::{ name = Some secretName, key }
          }
        }

--| The graph's password, once for the agents that read it and in the form the graph itself wants,
--| made together so they cannot disagree.
let graphSecrets =
      [ P.Secret::{
        , namespace = ns
        , name = "polar-graph-pw"
        , group = Some "graph"
        , keys =
          [ { mapKey = "secret"
            , mapValue = P.Value.Random { random = 16, id = Some "password" }
            }
          ]
        }
      , P.Secret::{
        , namespace = ns
        , name = "neo4j-secret"
        , group = Some "graph"
        , keys =
          [ { mapKey = "secret"
            , mapValue = P.Value.Template { template = "neo4j/{password}" }
            }
          ]
        }
      ]

let core
    : CoreValues.Type → { platform : Builder.Platform.Type, stages : List P.Stage.Type }
    = λ(v : CoreValues.Type) →
        let issuer =
              Platform.makeCertIssuer
                Platform.CertIssuer::{
                , namespace = ns
                , image = v.images.certIssuer
                , imagePullPolicy = v.imagePullPolicy
                , storageClass = v.storageClass
                -- The one identity allowed to ask it to sign on another's behalf. The fork fixes this name.
                , internalCaller = "credentials-agent.${ns}.serviceaccount.cluster.local"
                }

        let graph =
              Platform.makeNeo4j
                Platform.Neo4j::{
                , namespace = ns
                , image = v.images.neo4j
                , imagePullPolicy = v.imagePullPolicy
                , storageClass = v.storageClass
                }

        -- What every one of these has in common: they take their certificates from the issuer directly
        -- (the credentials agent is what vouches for everyone else, and Cassini is what the escrow
        -- bootstrap travels through), none bootstraps an escrow key, and none waits for the broker.
        let coreAgent =
              Builder.BuilderAgent::{
              , name = ""
              , namespace = ns
              , image = ""
              , issuer = Some (Builder.Issuer.CertIssuer certIssuerUrl)
              , waitForBroker = False
              , skipEscrow = True
              }

        let cassini =
                coreAgent
              // { name = "cassini"
                 , image = v.images.cassini
                 , provides = [] : List Text
                 , shutdownBudgetSecs = 20
                 , cert = Builder.CertKind.Server C.cassiniDNSName
                 , client = False
                 , port = Some C.cassiniTcpPort
                 , settings =
                   [ setting "TLS_SERVER_CERT_CHAIN" C.certPaths.cert
                   , setting "TLS_SERVER_KEY" C.certPaths.key
                   , setting "TLS_CA_CERT" C.certPaths.ca
                   , setting "CASSINI_BIND_ADDR" "0.0.0.0:${Natural/show C.cassiniTcpPort}"
                   , secretSetting "CASSINI_SHUTDOWN_TOKEN" "cassini-shutdown-token" "token"
                   ]
                 }

        let credentialsAgent =
                coreAgent
              // { name = "credentials-agent"
                 , image = v.images.credentialsAgent
                 , shutdownBudgetSecs = 50
                 , data = Some
                   { claim = "credentials-agent-data"
                   , mountPath = "/var/lib/credentials-agent"
                   }
                 , settings =
                   [ setting "CREDENTIALS_AGENT_CERT_ISSUER_URL" certIssuerUrl
                   -- The graph is a projection of the ledger, and the agent runs without it; its
                   -- settings are required all the same.
                   , setting "GRAPH_ENDPOINT" "bolt://polar-db-svc.${ns}.svc.cluster.local:7687"
                   , setting "GRAPH_DB" "neo4j"
                   , setting "GRAPH_USER" "neo4j"
                   , secretSetting "GRAPH_PASSWORD" "polar-graph-pw" "secret"
                   ]
                 }

        let observer =
                coreAgent
              // { name = "credentials-agent-observer"
                 , image = v.images.credentialsAgentObserver
                 , shutdownBudgetSecs = 40
                 , port = Some 8080
                 }

        let account = λ(name : Text) → kubernetes.Resource.ServiceAccount (Builder.makeServiceAccount ns name)

        in  { platform = platform v
            , stages =
              [ P.operatorStage
                  { namespace = "polar-operator"
                  , image = v.images.operator
                  , imagePullPolicy = v.imagePullPolicy
                  }
              , P.Stage::{
                , name = "issuer"
                , description = Some "the polar namespace, the secrets, and the certificate issuer"
                , shared = True
                , secrets = [ P.randomSecret ns "cassini-shutdown-token" "token" 24 ] # graphSecrets
                , objects =
                      P.k8s
                        (   [ kubernetes.Resource.Namespace
                                kubernetes.Namespace::{
                                , metadata = kubernetes.ObjectMeta::{ name = Some ns }
                                }
                            ]
                          # issuer.resources
                        )
                    # P.agents [ issuer.agent ]
                , waits = [ P.waitForAgent ns "cert-issuer" ]
                }
              , P.Stage::{
                , name = "broker"
                , description = Some "Cassini, the message broker"
                , shared = True
                , objects =
                      P.k8s
                        [ account "cassini"
                        , kubernetes.Resource.Service
                            (Builder.makeServiceFor ns C.cassiniServiceName "cassini" C.cassiniTcpPort)
                        ]
                    # P.agents [ Builder.makePolarAgent (platform v) cassini ]
                , waits = [ P.waitForAgent ns "cassini" ]
                }
              , P.Stage::{
                , name = "credentials"
                , description = Some "the credentials agent and its observer"
                , shared = True
                , objects =
                      P.k8s
                        [ account "credentials-agent"
                        , account "credentials-agent-observer"
                        , kubernetes.Resource.PersistentVolumeClaim
                            (Builder.makePvc ns "credentials-agent-data" "1Gi" v.storageClass)
                        , kubernetes.Resource.Service
                            (Builder.makeService ns "credentials-agent-observer" 8080)
                        ]
                    # P.agents
                        [ Builder.makePolarAgent (platform v) credentialsAgent
                        , Builder.makePolarAgent (platform v) observer
                        ]
                , waits =
                  [ P.waitForAgent ns "credentials-agent"
                  , P.waitForAgent ns "credentials-agent-observer"
                  ]
                }
              , P.Stage::{
                , name = "graph"
                , description = Some "the graph, with a password (development grade: no TLS)"
                , shared = True
                , objects = P.k8s graph.resources # P.agents [ graph.agent ]
                , waits = [ P.waitForAgent ns "polar-neo4j" ]
                }
              ]
            }

in  { CoreValues, core, platform }
