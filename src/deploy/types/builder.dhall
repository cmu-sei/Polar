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

-- The API group and version of the operator's resource (polar-operator-common::crd).
let apiVersion = "polar.sei.cmu.edu/v1alpha1"

let kind = "PolarAgent"

-- The label the operator puts on every pod of an agent: a Service selects on it.
let agentLabel = "polar.sei.cmu.edu/agent"

-- Where the projected service-account token is mounted for polar-init (its default path).
let tokenDir = "/workspace"

let tokenPath = "${tokenDir}/token"

let healthFile = "/tmp/polar-health.json"

--| What the platform gives every agent, stated once by the consumer. These are facts about the
--| cluster, not about any agent.
let Platform =
      { Type =
          { cassiniAddr : Text
          , cassiniServerName : Text
          , certIssuerUrl : Text
          , audience : Text
          , polarInitImage : Text
          , polarInitEnv : List kubernetes.EnvVar.Type
          , tokenExpirySecs : Natural
          , imageTag : Optional Text
          }
      , default =
        { audience = C.certIssuerAudience
        , polarInitEnv = [] : List kubernetes.EnvVar.Type
        , tokenExpirySecs = C.certTokenExpiry
        , imageTag = None Text
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
        }
      }

--| The pod additions the operator merges over its hardened defaults (polar-operator-common::crd,
--| `PodOverlay`).
let PodOverlay =
      { runAsUser : Natural
      , initContainers : List kubernetes.Container.Type
      , volumes : List kubernetes.Volume.Type
      , volumeMounts : List kubernetes.VolumeMount.Type
      , env : List kubernetes.EnvVar.Type
      , ports : List kubernetes.ContainerPort.Type
      , livenessProbe : kubernetes.Probe.Type
      }

let PolarAgentSpec =
      { image : Text
      , agent : Optional Text
      , needs : List Text
      , shutdownBudgetSecs : Natural
      , serviceAccount : Optional Text
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

--| The five variables every Cassini client reads, plus what the health actor watches.
let clientEnv
    : Platform.Type → BuilderAgent.Type → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        [ envValue "TLS_CA_CERT" C.certPaths.ca
        , envValue "TLS_CLIENT_CERT" C.certPaths.cert
        , envValue "TLS_CLIENT_KEY" C.certPaths.key
        , envValue "BROKER_ADDR" platform.cassiniAddr
        , envValue "CASSINI_SERVER_NAME" platform.cassiniServerName
        , envValue "POLAR_HEALTH_FILE" healthFile
        , envValue "POLAR_HEALTH_CERTS" C.certPaths.cert
        , envValue
            "POLAR_HEALTH_DEP_ENDPOINTS"
            "${platform.cassiniAddr}:${Natural/show agent.health.depTtlSecs}"
        ]

--| The init container that obtains the pod's client certificate.
let polarInit
    : Platform.Type → BuilderAgent.Type → kubernetes.Container.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        kubernetes.Container::{
        , name = "polar-init"
        , image = Some platform.polarInitImage
        , args = Some
          [ "--check-dep-cert"
          , "${platform.cassiniAddr}:${Natural/show agent.health.depTtlSecs}"
          , "--cert"
          , "client:${C.certDir}:ecdsa-p256"
          ]
        , env = Some
            (   [ envValue "POLAR_INIT_CERT_ISSUER_URL" platform.certIssuerUrl
                , envValue "POLAR_INIT_TOKEN_PATH" tokenPath
                ]
              # platform.polarInitEnv
            )
        , volumeMounts = Some
          [ kubernetes.VolumeMount::{
            , name = tokenVolumeName
            , mountPath = tokenDir
            , readOnly = Some True
            }
          , kubernetes.VolumeMount::{
            , name = certVolumeName
            , mountPath = C.certDir
            }
          ]
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
          , pod =
            { runAsUser = agent.runAsUser
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
            , livenessProbe = livenessProbe agent
            }
          }
        }

--| The Service for an agent that answers requests. It selects the pods the operator labels.
let makeService
    : Text → Text → Natural → kubernetes.Service.Type
    = λ(namespace : Text) →
      λ(name : Text) →
      λ(port : Natural) →
        kubernetes.Service::{
        , metadata = kubernetes.ObjectMeta::{
          , name = Some name
          , namespace = Some namespace
          }
        , spec = Some kubernetes.ServiceSpec::{
          , selector = Some [ { mapKey = agentLabel, mapValue = name } ]
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

in  { Platform
    , Health
    , Data
    , BuilderAgent
    , PodOverlay
    , PolarAgentSpec
    , PolarAgentResource
    , makePolarAgent
    , makeService
    , makePvc
    , makeServiceAccount
    , agentLabel
    , optionalList
    }
