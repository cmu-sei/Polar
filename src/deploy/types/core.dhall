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

  Everything is in one namespace, `polar`, the operator included: the first stage creates it.

  The stages are in the order the core needs, each waiting on the one before: the operator and its CRD;
  the issuer, which every pod's init container asks for a certificate; Cassini; the credentials agent and
  its observer; the graph.
-}
let kubernetes = ./kubernetes.dhall

let C = ./lib-constants.dhall

let Builder = ./builder.dhall

let Platform = ./platform.dhall

let P = ./plan.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let Settings = ./settings.dhall

let setting = Settings.setting

let secretSetting = Settings.secretSetting


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
              , gitServer : Text
              , schedulerProcessor : Text
              , schedulerObserver : Text
              }
          , imagePullPolicy : Optional Text
          , storageClass : Optional Text
          -- The custody daemon's image: with it, every pod's mTLS keys are the daemon's (a native sidecar), and an
          -- agent that encrypts can run at all. `None`: key files, and no agent that encrypts.
          , custodyImage : Optional Text
          -- The ConfigMap, in the namespace of every pod that has a certificate, that holds the cluster's CA. With
          -- `caSecret`, the plan makes the one in the core's namespace; the others are the application's to declare
          -- (`Plan.pinnedCa`), in the stage that makes their namespace.
          , pinnedCa : Optional Text
          -- The Secret that holds the CA and its key, for the issuer to load; the plan makes it, once, and never
          -- replaces it (to replace the CA, delete the Secret: every certificate is then reissued). `None`: the issuer
          -- makes its own CA (nothing can pin it, so not for a platform with the daemon, or one whose pods are
          -- issued through the credentials agent).
          , caSecret : Optional Text
          -- The people who may write to the configuration git server, and so author the signed documents
          -- the scheduler carries: the DNS name in each one's certificate, which only exists once the
          -- credentials agent has provisioned them (the first administrator, see the runbook). Empty
          -- until then.
          , gitServerAdmins : List Text
          -- What Cassini's topic rules are written from, as JSON the fork's catalog would generate from the agents'
          -- scaffolds (Blitz: infra/tools/authority.nu). The authority model goes to the credentials agent, which
          -- writes each workload's topics into the certificates it issues; the floor goes to cert-issuer, which does
          -- the same for the bootstrap services. `None`: no certificate carries a topic.
          , authorityModel : Optional Text
          , issuanceFloor : Optional Text
          -- The workloads the scheduler publishes a configuration topic for, comma-separated: the agents that read a
          -- configuration document. The scheduler pair computes the topics it may use from it, so it must be what the
          -- model's certificates carry.
          , servedWorkloads : Optional Text
          }
      }

let ns = C.polarNamespace

let graphSettings = Settings.graphSettings ns

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
        , custodyImage = v.custodyImage
        , pinnedCa = v.pinnedCa
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
                , caSecret = v.caSecret
                , floor = v.issuanceFloor
                }

        let graph =
              Platform.makeNeo4j
                Platform.Neo4j::{
                , namespace = ns
                , image = v.images.neo4j
                , imagePullPolicy = v.imagePullPolicy
                , storageClass = v.storageClass
                }

        -- Whether the pods' keys are the daemon's, here: a server's key variable is left out then, since the broker
        -- and the git server read a key file or the daemon's socket, exactly one.
        let custody = merge { Some = λ(_ : Text) → True, None = False } v.custodyImage

        let keyFile =
              λ(name : Text) →
              λ(path : Text) →
                if    custody
                then  [] : List kubernetes.EnvVar.Type
                else  [ setting name path ]

        -- What every one of these has in common: they take their certificates from the issuer directly
        -- (the credentials agent is what vouches for everyone else), none holds an escrow key, and none waits
        -- for the broker.
        let coreAgent =
              Builder.BuilderAgent::{
              , name = ""
              , namespace = ns
              , image = ""
              , issuer = Some (Builder.Issuer.CertIssuer certIssuerUrl)
              , waitForBroker = False
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
                   , setting "TLS_CA_CERT" C.certPaths.ca
                   , setting "CASSINI_BIND_ADDR" "0.0.0.0:${Natural/show C.cassiniTcpPort}"
                   , secretSetting "CASSINI_SHUTDOWN_TOKEN" "cassini-shutdown-token" "token"
                   ]
                   # keyFile "TLS_SERVER_KEY" C.certPaths.key
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
                   ]
                   # graphSettings
                   # merge
                       { Some =
                           λ(model : Text) →
                             [ setting "CREDENTIALS_AGENT_AUTHORITY_MODEL" model ]
                       , None = [] : List kubernetes.EnvVar.Type
                       }
                       v.authorityModel
                 }

        let observer =
                coreAgent
              // { name = "credentials-agent-observer"
                 , image = v.images.credentialsAgentObserver
                 , shutdownBudgetSecs = 40
                 , port = Some 8080
                 }

        -- The configuration git server: smart HTTP over mTLS, a client certificate required of everyone.
        -- It makes and seeds the repository itself, once, from a seed baked into its image, and answers
        -- only to the identities its access list names: here, the scheduler's observer, to read.
        let gitServerDns = "polar-git-server.${ns}.svc.cluster.local"

        let gitServerDir = "/home/polar/git-server"

        let schedulerObserverIdentity = "polar-scheduler-observer.${ns}.serviceaccount.cluster.local"

        let gitServer =
              Builder.BuilderAgent::{
              , name = "polar-git-server"
              , namespace = ns
              , image = v.images.gitServer
              , shutdownBudgetSecs = 45
              , port = Some 8443
              , data = Some
                { claim = "polar-git-server-data"
                , mountPath = "/var/lib/polar-git-server"
                }
              -- Its server certificate, besides the client certificate it uses to reach Cassini.
              , extraCerts =
                [ { name = "git-server-tls"
                  , kind = Builder.CertKind.Server gitServerDns
                  , dir = gitServerDir
                  }
                ]
              , settings =
                [ setting "POLAR_GIT_SERVER_CERT" "${gitServerDir}/cert.pem"
                , setting "POLAR_GIT_SERVER_CA_CERT" "${gitServerDir}/ca.pem"
                , setting "POLAR_GIT_SERVER_REPO_ROOT" "/var/lib/polar-git-server/repos"
                , setting "POLAR_GIT_SERVER_PORT" "8443"
                , setting
                    "POLAR_GIT_SERVER_REPOS"
                    "{\"admins\":[],\"repos\":[{\"name\":\"polar-schedules\",\"readers\":[\"${schedulerObserverIdentity}\"],\"writers\":[],\"seed\":\"polar-schedules\"}]}"
                ]
                # keyFile "POLAR_GIT_SERVER_KEY" "${gitServerDir}/key.pem"
                # ( if    Prelude.List.null Text v.gitServerAdmins
                    then  [] : List kubernetes.EnvVar.Type
                    else  [ setting
                              "POLAR_GIT_SERVER_ADMINS"
                              (Prelude.Text.concatSep "," v.gitServerAdmins)
                          ]
                  )
              }

        -- The scheduler. The processor records schedules in the graph and tells the agents they affect;
        -- the observer watches the schedules repository and publishes what changes in it. Both publish a
        -- configuration topic per workload that reads a document, which they are told (the topic family).
        let served =
              merge
                { Some = λ(names : Text) → [ setting "POLAR_SERVED_WORKLOADS" names ]
                , None = [] : List kubernetes.EnvVar.Type
                }
                v.servedWorkloads

        let schedulerProcessor =
              Builder.BuilderAgent::{
              , name = "polar-scheduler"
              , namespace = ns
              , image = v.images.schedulerProcessor
              , provides = [ "cassini", "graph-write" ]
              , shutdownBudgetSecs = 50
              , settings = graphSettings # served
              }

        let schedulerObserver =
              Builder.BuilderAgent::{
              , name = "polar-scheduler-observer"
              , namespace = ns
              , image = v.images.schedulerObserver
              , shutdownBudgetSecs = 20
              -- git reads its client key from a file (libcurl), so this one pod stays on key files until the fork
              -- can sign for git (ACE's catalog says the same: `custody: false`).
              , custody = False
              -- The checkout of the schedules repository does not outlive the pod: it is cloned again.
              , scratch =
                [ { name = "checkout", mountPath = "/var/lib/polar-scheduler-observer" } ]
              , settings =
                [ setting
                    "POLAR_SCHEDULER_REMOTE_URL"
                    "https://${gitServerDns}:8443/polar-schedules.git"
                , setting
                    "POLAR_SCHEDULER_LOCAL_PATH"
                    "/var/lib/polar-scheduler-observer/polar-schedules"
                -- Its own certificate, the one it reaches Cassini with, is what the git server checks.
                , setting "POLAR_SCHEDULER_GIT_CA_CERT" C.certPaths.ca
                , setting "POLAR_SCHEDULER_GIT_CLIENT_CERT" C.certPaths.cert
                , setting "POLAR_SCHEDULER_GIT_CLIENT_KEY" C.certPaths.key
                , setting "POLAR_SCHEDULER_SYNC_INTERVAL" "300"
                -- git wants a home to write to, and only /tmp is writable.
                , setting "HOME" "/tmp"
                ]
                # served
              }

        let account = λ(name : Text) → kubernetes.Resource.ServiceAccount (Builder.makeServiceAccount ns name)

        in  { platform = platform v
            , stages =
              [ P.operatorStage
                  -- Beside everything else: the core lives in one namespace, the operator in it.
                  { namespace = ns
                  , image = v.images.operator
                  , imagePullPolicy = v.imagePullPolicy
                  }
              ]
              # ( merge
                    { Some =
                        λ(secret : Text) →
                          [ P.Stage::{
                            , name = "ca"
                            , description = Some
                                "the cluster's CA, made once, with the certificate every namespace's agents verify against"
                            , shared = True
                            , secrets =
                              [ P.certificateAuthority
                                  { namespace = ns
                                  , name = secret
                                  , commonName = C.ca.commonName
                                  , validityDays = C.ca.validityDays
                                  }
                              ]
                            , configMaps =
                                merge
                                  { Some =
                                      λ(pinned : Text) →
                                        [ P.pinnedCa
                                            { namespace = ns
                                            , name = pinned
                                            , secret = { namespace = ns, name = secret }
                                            }
                                        ]
                                  , None = [] : List P.ConfigMap.Type
                                  }
                                  v.pinnedCa
                            }
                          ]
                    , None = [] : List P.Stage.Type
                    }
                    v.caSecret
                )
              # [ P.Stage::{
                , name = "issuer"
                , description = Some "the secrets, and the certificate issuer"
                , shared = True
                , secrets = [ P.randomSecret ns "cassini-shutdown-token" "token" 24 ] # graphSecrets
                , objects = P.k8s issuer.resources # P.agents [ issuer.agent ]
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
                        -- Where the first administrator proves control of the cluster. Nothing runs
                        -- here, no role or binding may ever name this account, and no pod mounts its
                        -- token: whoever can mint one (`polar-admin-grant bootstrap`, with the
                        -- cluster administrator's own kubeconfig) controls the cluster. The credentials
                        -- agent accepts exactly this identity for the bootstrap claim.
                        , kubernetes.Resource.Namespace
                            kubernetes.Namespace::{
                            , metadata = kubernetes.ObjectMeta::{ name = Some "polar-bootstrap" }
                            }
                        , kubernetes.Resource.ServiceAccount
                            kubernetes.ServiceAccount::{
                            , metadata = kubernetes.ObjectMeta::{
                              , name = Some "polar-admin-bootstrap"
                              , namespace = Some "polar-bootstrap"
                              }
                            , automountServiceAccountToken = Some False
                            }
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
              , P.Stage::{
                , name = "git"
                , description = Some "the configuration git server, which makes and seeds the schedules repository"
                , shared = True
                , objects =
                      P.k8s
                        [ account "polar-git-server"
                        , kubernetes.Resource.PersistentVolumeClaim
                            (Builder.makePvc ns "polar-git-server-data" "1Gi" v.storageClass)
                        , kubernetes.Resource.Service (Builder.makeService ns "polar-git-server" 8443)
                        ]
                    # P.agents [ Builder.makePolarAgent (platform v) gitServer ]
                , waits = [ P.waitForAgent ns "polar-git-server" ]
                }
              -- After the git server, and not with it: the observer ends if the server is not there,
              -- and the operator holds an agent that ends too often too soon.
              , P.Stage::{
                , name = "scheduler"
                , description = Some "the scheduler: its processor, and the observer that watches the schedules repository"
                , shared = True
                , objects =
                      P.k8s
                        [ account "polar-scheduler", account "polar-scheduler-observer" ]
                    # P.agents
                        [ Builder.makePolarAgent (platform v) schedulerProcessor
                        , Builder.makePolarAgent (platform v) schedulerObserver
                        ]
                , waits =
                  [ P.waitForAgent ns "polar-scheduler"
                  , P.waitForAgent ns "polar-scheduler-observer"
                  ]
                }
              ]
            }

in  { CoreValues, core, platform }
