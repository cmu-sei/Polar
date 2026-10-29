{-
  types/builder.dhall — agents built on polar-agent-builder, rendered as PolarAgent resources.

  A builder agent is an ordinary process that follows the agent contract: it declares in its own
  code what it needs (`--scaffold`), refuses to start when the deployment does not provide it, drains
  within a shutdown budget, and ends (always non-zero) when it wants a replacement. The polar-operator
  keeps one instance of each alive and records how each one ended. This file is the other half: it
  says what a pod for such an agent looks like, once, so a consumer writes only what is theirs.

  What a consumer writes (see `BuilderAgent.default`):

    BuilderAgent::{ name = "blitz-portal", namespace = "blitz", image = "...", settings = [ ... ] }

  What this file adds around it, and why:

    cert init container   polar-init obtains the pod's client certificate from cert-issuer with the
                          pod's projected service-account token, and first waits until the broker's
                          own certificate has life left in it
    health                POLAR_HEALTH_* so the agent's health actor watches its certificate, and a
                          liveness probe that fails when the health file stops being written
    TLS and broker env    the five variables every Cassini client reads

  What it deliberately does not do (they belong to the consumer, not the library):

    order of deployment, differences between environments, and secrets. A secret is referenced by
    name (`kubernetes.EnvVarSource`), never created or embedded here.

  Rendering: a `PolarAgent` is a custom resource, which `kubernetes.Resource` does not list, so
  render the agents in a file of their own (a `List PolarAgentResource`, `dhall-to-yaml --documents`)
  and the Services and claims that go with them in another (`List kubernetes.Resource`).
-}
let kubernetes = ./kubernetes.dhall

let C = ./lib-constants.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

-- The API group and version of the operator's resource (polar-operator-common::crd).
let apiVersion = "polar.sei.cmu.edu/v1alpha1"

let kind = "PolarAgent"

-- The label the operator puts on every pod of an agent: a Service selects on it.
let agentLabel = "polar.sei.cmu.edu/agent"

-- Where the projected service-account token is mounted for polar-init (its default path).
let tokenDir = "/workspace"

let tokenPath = "${tokenDir}/token"

let healthFile = "/tmp/polar-health.json"

--| Where a pod's certificates come from.
--|
--| `CertIssuer url`: polar-init presents the pod's service-account token to the issuer directly. Only
--| for workloads the platform has to start before anything else (the issuer's own bootstrap identities,
--| or any workload while its issuance floor is off).
--| `CredentialsAgent url`: polar-init asks the credentials agent's observer, which vouches for the
--| request. The way for everything else, and the only way a certificate ends up in the agent's ledger,
--| which the escrow bootstrap needs.
let Issuer = < CertIssuer : Text | CredentialsAgent : Text >

--| The kind of certificate a pod asks for. A server certificate carries the DNS name clients dial.
let CertKind = < Client | Server : Text >

--| What the platform gives every agent, stated once by the consumer. These are facts about the
--| cluster, not about any agent.
let Platform =
      { Type =
          { cassiniAddr : Text
          , cassiniServerName : Text
          , issuer : Issuer
          , audience : Text
          , polarInitImage : Text
          , polarInitEnv : List kubernetes.EnvVar.Type
          , tokenExpirySecs : Natural
          , imagePullPolicy : Optional Text
          }
      , default =
        { audience = C.certIssuerAudience
        , polarInitEnv = [] : List kubernetes.EnvVar.Type
        , tokenExpirySecs = C.certTokenExpiry
        , imagePullPolicy = None Text
        }
      }

--| How an agent's health actor and liveness probe behave. The values are the builder's own
--| defaults and what the Polar pipeline has used; override per agent where an agent differs.
let Health =
      { Type =
          { depTtlSecs : Natural
          , initialDelaySecs : Natural
          , periodSecs : Natural
          , failureThreshold : Natural
          , timeoutSecs : Natural
          }
      , default =
        { depTtlSecs = 300
        , initialDelaySecs = 150
        , periodSecs = 30
        , failureThreshold = 2
        , timeoutSecs = 5
        }
      }

--| A volume the agent keeps its state on: an existing claim, mounted where the agent expects it.
let Data = { claim : Text, mountPath : Text }

--| Another certificate the agent is given, in a directory of its own: the git server presents a server
--| certificate on its own port besides the client certificate it uses to reach Cassini.
let ExtraCert = { name : Text, kind : CertKind, dir : Text }

--| Space that does not outlive the pod, writable, for an agent whose root filesystem is read-only.
let Scratch = { name : Text, mountPath : Text }

--| What a consumer says about one agent.
let BuilderAgent =
      { Type =
          { name : Text
          , namespace : Text
          , image : Text
          , agent : Optional Text
          , provides : List Text
          , shutdownBudgetSecs : Natural
          , serviceAccount : Optional Text
          , runAsUser : Natural
          , settings : List kubernetes.EnvVar.Type
          , port : Optional Natural
          , data : Optional Data
          , health : Health.Type
          , cert : CertKind
          , issuer : Optional Issuer
          , client : Bool
          , waitForBroker : Bool
          , skipEscrow : Bool
          , extraCerts : List ExtraCert
          , scratch : List Scratch
          }
      , default =
        { agent = None Text
        , provides = [ "cassini" ]
        , shutdownBudgetSecs = 20
        , serviceAccount = None Text
        , runAsUser = 1000
        , settings = [] : List kubernetes.EnvVar.Type
        , port = None Natural
        , data = None Data
        , health = Health.default
        -- A Cassini client that waits for the broker's certificate before it starts and registers
        -- its own escrow key with the credentials agent. The platform's own pieces turn these off.
        , cert = CertKind.Client
        -- Overrides the platform's issuer for this agent: the platform's own pieces take their
        -- certificates from the issuer directly, since they are what the credentials agent runs on.
        , issuer = None Issuer
        , client = True
        , waitForBroker = True
        , skipEscrow = False
        , extraCerts = [] : List ExtraCert
        , scratch = [] : List Scratch
        }
      }

--| The pod additions the operator merges over its hardened defaults (polar-operator-common::crd,
--| `PodOverlay`).
let PodOverlay =
      { runAsUser : Natural
      , fsGroup : Optional Natural
      , imagePullPolicy : Optional Text
      , initContainers : List kubernetes.Container.Type
      , volumes : List kubernetes.Volume.Type
      , volumeMounts : List kubernetes.VolumeMount.Type
      , env : List kubernetes.EnvVar.Type
      , ports : List kubernetes.ContainerPort.Type
      , livenessProbe : Optional kubernetes.Probe.Type
      , readinessProbe : Optional kubernetes.Probe.Type
      , automountServiceAccountToken : Optional Bool
      , readOnlyRootFilesystem : Optional Bool
      }

let PolarAgentSpec =
      { image : Text
      , agent : Optional Text
      , needs : List Text
      , shutdownBudgetSecs : Natural
      , serviceAccount : Optional Text
      , command : Optional (List Text)
      , args : Optional (List Text)
      , pod : PodOverlay
      }

--| The `PolarAgent` resource itself.
let PolarAgentResource =
      { apiVersion : Text
      , kind : Text
      , metadata : kubernetes.ObjectMeta.Type
      , spec : PolarAgentSpec
      }

let certVolumeName = C.certVolumeName

let tokenVolumeName = C.saTokenVolumeName

let dataVolumeName = "data"

let envValue =
      λ(name : Text) →
      λ(value : Text) →
        kubernetes.EnvVar::{ name, value = Some value }

let nonRootDropAll =
      λ(uid : Natural) →
        kubernetes.SecurityContext::{
        , runAsUser = Some uid
        , runAsGroup = Some uid
        , runAsNonRoot = Some True
        , allowPrivilegeEscalation = Some False
        , capabilities = Some kubernetes.Capabilities::{
          , drop = Some [ "ALL" ]
          }
        }

--| The five variables every Cassini client reads.
let tlsEnv
    : Platform.Type → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
        [ envValue "TLS_CA_CERT" C.certPaths.ca
        , envValue "TLS_CLIENT_CERT" C.certPaths.cert
        , envValue "TLS_CLIENT_KEY" C.certPaths.key
        , envValue "BROKER_ADDR" platform.cassiniAddr
        , envValue "CASSINI_SERVER_NAME" platform.cassiniServerName
        ]

--| What the agent's health actor watches: its own certificate, and the broker's while the agent
--| depends on it.
let healthEnv
    : Platform.Type → BuilderAgent.Type → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
          [ envValue "POLAR_HEALTH_FILE" healthFile
          , envValue "POLAR_HEALTH_CERTS" C.certPaths.cert
          ]
        # ( if    agent.waitForBroker
            then  [ envValue
                      "POLAR_HEALTH_DEP_ENDPOINTS"
                      "${platform.cassiniAddr}:${Natural/show agent.health.depTtlSecs}"
                  ]
            else  [] : List kubernetes.EnvVar.Type
          )

let clientEnv
    : Platform.Type → BuilderAgent.Type → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
          ( if    agent.client
            then  tlsEnv platform
            else  [] : List kubernetes.EnvVar.Type
          )
        # healthEnv platform agent

--| The environment that says where polar-init gets certificates.
let issuerEnv
    : Issuer → List kubernetes.EnvVar.Type
    = λ(issuer : Issuer) →
        merge
          { CertIssuer =
              λ(url : Text) →
                [ envValue "POLAR_INIT_ISSUER" "cert-issuer"
                , envValue "POLAR_INIT_CERT_ISSUER_URL" url
                ]
          , CredentialsAgent =
              λ(url : Text) →
                [ envValue "POLAR_INIT_ISSUER" "credentials-agent"
                , envValue "POLAR_INIT_OBSERVER_URL" url
                ]
          }
          issuer

--| After issuing, polar-init registers an escrow keypair with the credentials agent over Cassini,
--| using the certificate it just obtained, so it needs the client environment and its token. The
--| platform's own pieces skip it: they are what it travels through.
let escrowEnv
    : Platform.Type → BuilderAgent.Type → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        if    agent.skipEscrow
        then  [ envValue "POLAR_INIT_SKIP_ESCROW_BOOTSTRAP" "true" ]
        else    tlsEnv platform
              # [ envValue "KUBERNETES_SA_TOKEN" tokenPath ]

let certArgumentIn
    : CertKind → Text → Text
    = λ(kind : CertKind) →
      λ(dir : Text) →
        merge
          { Client = "client:${dir}:ecdsa-p256"
          , Server = λ(san : Text) → "server:${dir}:ecdsa-p256:${san}"
          }
          kind

let certArgument
    : CertKind → Text
    = λ(kind : CertKind) → certArgumentIn kind C.certDir

let extraCertArguments
    : List ExtraCert → List Text
    = λ(certs : List ExtraCert) →
        Prelude.List.concatMap
          ExtraCert
          Text
          (λ(c : ExtraCert) → [ "--cert", certArgumentIn c.kind c.dir ])
          certs

let extraCertVolume
    : ExtraCert → kubernetes.Volume.Type
    = λ(c : ExtraCert) →
        kubernetes.Volume::{
        , name = c.name
        , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
        }

let scratchVolume
    : Scratch → kubernetes.Volume.Type
    = λ(s : Scratch) →
        kubernetes.Volume::{
        , name = s.name
        , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
        }

--| The init container that obtains the pod's certificate.
let polarInit
    : Platform.Type → BuilderAgent.Type → kubernetes.Container.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        kubernetes.Container::{
        , name = "polar-init"
        , image = Some platform.polarInitImage
        -- The image names its binary as a default command, not an entrypoint, so giving only
        -- `args` would replace it and Kubernetes would try to run "--cert".
        , command = Some [ "polar-init" ]
        , imagePullPolicy = platform.imagePullPolicy
        , args = Some
            (   ( if    agent.waitForBroker
                  then  [ "--check-dep-cert"
                        , "${platform.cassiniAddr}:${Natural/show agent.health.depTtlSecs}"
                        ]
                  else  [] : List Text
                )
              # [ "--cert", certArgument agent.cert ]
              # extraCertArguments agent.extraCerts
            )
        , env = Some
            (   issuerEnv
                  ( merge
                      { Some = λ(own : Issuer) → own, None = platform.issuer }
                      agent.issuer
                  )
              # [ envValue "POLAR_INIT_TOKEN_PATH" tokenPath ]
              # escrowEnv platform agent
              # platform.polarInitEnv
            )
        , volumeMounts = Some
          ( [ kubernetes.VolumeMount::{
            , name = tokenVolumeName
            , mountPath = tokenDir
            , readOnly = Some True
            }
          , kubernetes.VolumeMount::{
            , name = certVolumeName
            , mountPath = C.certDir
            }
          ]
          # Prelude.List.map
              ExtraCert
              kubernetes.VolumeMount.Type
              ( λ(c : ExtraCert) →
                  kubernetes.VolumeMount::{ name = c.name, mountPath = c.dir }
              )
              agent.extraCerts
          )
        , securityContext = Some (nonRootDropAll agent.runAsUser)
        }

let livenessProbe
    : BuilderAgent.Type → kubernetes.Probe.Type
    = λ(agent : BuilderAgent.Type) →
        kubernetes.Probe::{
        , exec = Some kubernetes.ExecAction::{
          , command = Some [ "polar-healthcheck" ]
          }
        , initialDelaySeconds = Some agent.health.initialDelaySecs
        , periodSeconds = Some agent.health.periodSecs
        , failureThreshold = Some agent.health.failureThreshold
        , timeoutSeconds = Some agent.health.timeoutSecs
        }

let optionalList =
      λ(a : Type) →
      λ(o : Optional a) →
        merge { Some = λ(x : a) → [ x ], None = [] : List a } o

--| Render one agent as a `PolarAgent` resource.
let makePolarAgent
    : Platform.Type → BuilderAgent.Type → PolarAgentResource
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        { apiVersion
        , kind
        , metadata = kubernetes.ObjectMeta::{
          , name = Some agent.name
          , namespace = Some agent.namespace
          }
        , spec =
          { image = agent.image
          , agent = agent.agent
          , needs = agent.provides
          , shutdownBudgetSecs = agent.shutdownBudgetSecs
          , serviceAccount = agent.serviceAccount
          , command = None (List Text)
          , args = None (List Text)
          , pod =
            { automountServiceAccountToken = None Bool
            , readOnlyRootFilesystem = None Bool
            , runAsUser = agent.runAsUser
            -- The group owns the volumes, so a claim is writable by the user the agent runs as.
            , fsGroup = Some agent.runAsUser
            , imagePullPolicy = platform.imagePullPolicy
            , initContainers = [ polarInit platform agent ]
            , volumes =
                  [ kubernetes.Volume::{
                    , name = tokenVolumeName
                    , projected = Some kubernetes.ProjectedVolumeSource::{
                      , sources = Some
                        [ kubernetes.VolumeProjection::{
                          , serviceAccountToken = Some kubernetes.ServiceAccountTokenProjection::{
                            , audience = Some platform.audience
                            , expirationSeconds = Some platform.tokenExpirySecs
                            , path = "token"
                            }
                          }
                        ]
                      }
                    }
                  , kubernetes.Volume::{
                    , name = certVolumeName
                    , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
                    }
                  ]
                # Prelude.List.map ExtraCert kubernetes.Volume.Type extraCertVolume agent.extraCerts
                # Prelude.List.map Scratch kubernetes.Volume.Type scratchVolume agent.scratch
                # merge
                    { Some =
                        λ(d : Data) →
                          [ kubernetes.Volume::{
                            , name = dataVolumeName
                            , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                              , claimName = d.claim
                              }
                            }
                          ]
                    , None = [] : List kubernetes.Volume.Type
                    }
                    agent.data
            , volumeMounts =
                  [ kubernetes.VolumeMount::{
                    , name = certVolumeName
                    , mountPath = C.certDir
                    , readOnly = Some True
                    }
                  ]
                # Prelude.List.map
                    ExtraCert
                    kubernetes.VolumeMount.Type
                    ( λ(c : ExtraCert) →
                        kubernetes.VolumeMount::{
                        , name = c.name
                        , mountPath = c.dir
                        , readOnly = Some True
                        }
                    )
                    agent.extraCerts
                # Prelude.List.map
                    Scratch
                    kubernetes.VolumeMount.Type
                    ( λ(s : Scratch) →
                        kubernetes.VolumeMount::{ name = s.name, mountPath = s.mountPath }
                    )
                    agent.scratch
                # merge
                    { Some =
                        λ(d : Data) →
                          [ kubernetes.VolumeMount::{
                            , name = dataVolumeName
                            , mountPath = d.mountPath
                            }
                          ]
                    , None = [] : List kubernetes.VolumeMount.Type
                    }
                    agent.data
            , env = clientEnv platform agent # agent.settings
            , ports =
                merge
                  { Some =
                      λ(p : Natural) →
                        [ kubernetes.ContainerPort::{ containerPort = p } ]
                  , None = [] : List kubernetes.ContainerPort.Type
                  }
                  agent.port
            , livenessProbe = Some (livenessProbe agent)
            , readinessProbe = None kubernetes.Probe.Type
            }
          }
        }

--| The Service for an agent that answers requests, under a name of its own: some are dialled by a
--| name other than the agent's (Cassini is `cassini-ip-svc`). It selects the pods the operator labels.
let makeServiceFor
    : Text → Text → Text → Natural → kubernetes.Service.Type
    = λ(namespace : Text) →
      λ(serviceName : Text) →
      λ(agentName : Text) →
      λ(port : Natural) →
        kubernetes.Service::{
        , metadata = kubernetes.ObjectMeta::{
          , name = Some serviceName
          , namespace = Some namespace
          }
        , spec = Some kubernetes.ServiceSpec::{
          , selector = Some [ { mapKey = agentLabel, mapValue = agentName } ]
          , type = Some "ClusterIP"
          , ports = Some
            [ kubernetes.ServicePort::{
              , name = Some "http"
              , port
              , targetPort = Some (kubernetes.NatOrString.Nat port)
              }
            ]
          }
        }

--| The Service for an agent that answers requests, named for the agent.
let makeService
    : Text → Text → Natural → kubernetes.Service.Type
    = λ(namespace : Text) →
      λ(name : Text) →
      λ(port : Natural) →
        makeServiceFor namespace name name port

--| A claim for an agent's state. `storageClass` is the cluster's to choose, so it is a parameter.
let makePvc
    : Text → Text → Text → Optional Text → kubernetes.PersistentVolumeClaim.Type
    = λ(namespace : Text) →
      λ(name : Text) →
      λ(size : Text) →
      λ(storageClass : Optional Text) →
        kubernetes.PersistentVolumeClaim::{
        , metadata = kubernetes.ObjectMeta::{
          , name = Some name
          , namespace = Some namespace
          }
        , spec = Some kubernetes.PersistentVolumeClaimSpec::{
          , accessModes = Some [ "ReadWriteOnce" ]
          , storageClassName = storageClass
          , resources = Some kubernetes.VolumeResourceRequirements::{
            , requests = Some [ { mapKey = "storage", mapValue = size } ]
            }
          }
        }

--| The identity an agent runs as. The operator names it after the agent unless told otherwise.
let makeServiceAccount
    : Text → Text → kubernetes.ServiceAccount.Type
    = λ(namespace : Text) →
      λ(name : Text) →
        kubernetes.ServiceAccount::{
        , metadata = kubernetes.ObjectMeta::{
          , name = Some name
          , namespace = Some namespace
          }
        , automountServiceAccountToken = Some False
        }

in  { Issuer
    , CertKind
    , ExtraCert
    , Scratch
    , Platform
    , Health
    , Data
    , BuilderAgent
    , PodOverlay
    , PolarAgentSpec
    , PolarAgentResource
    , makePolarAgent
    , makeService
    , makeServiceFor
    , makePvc
    , makeServiceAccount
    , agentLabel
    , apiVersion
    , kind
    , optionalList
    }
