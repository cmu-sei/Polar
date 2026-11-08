-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

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
          -- The custody daemon's image. `Some`: a pod's mTLS keys are the daemon's, unless its agent opts out
          -- (`BuilderAgent.custody`), and the pod has the daemon as a native sidecar. `None`: key files, as before
          -- custody, and nothing about a pod changes for it.
          , custodyImage : Optional Text
          -- The ConfigMap, in each namespace that has agents, whose `ca.pem` is the cluster's CA: what polar-init
          -- verifies the certificates it is issued against and the daemon judges them by. `polar-init` requires it
          -- of an agent whose certificates come through the credentials agent, so it is `Some` wherever they do.
          , pinnedCa : Optional Text
          -- The credentials agent's identity (`<name>.<namespace>.serviceaccount.<domain>`): polar-init believes a
          -- session offer only if a certificate naming it, under the CA, signed it.
          , credentialsAgentIdentity : Text
          }
      , default =
        { audience = C.certIssuerAudience
        , polarInitEnv = [] : List kubernetes.EnvVar.Type
        , tokenExpirySecs = C.certTokenExpiry
        , imagePullPolicy = None Text
        , custodyImage = None Text
        , pinnedCa = None Text
        , credentialsAgentIdentity =
            "credentials-agent.${C.polarNamespace}.serviceaccount.cluster.local"
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

--| A ConfigMap the plan makes, mounted read-only where the agent reads it: its files are the ConfigMap's keys.
let ConfigFiles = { configMap : Text, mountPath : Text }

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
          -- The agent opens what is sealed to its escrow key (the `encryption` capability). Its key is the
          -- custody daemon's custodian's to fetch, so it cannot run without the daemon.
          , encrypts : Bool
          -- Whether the pod's keys are the custody daemon's, when the platform has the daemon at all. An agent
          -- whose code can only read a key file (the scheduler observer, whose git reads it) says `False`.
          , custody : Bool
          , extraCerts : List ExtraCert
          , scratch : List Scratch
          , configFiles : List ConfigFiles
          -- A Secret whose `ca-bundle.crt` key is the trust store of the agent's TLS clients
          -- (`SSL_CERT_FILE`), for a network that inspects TLS and signs with its own authority. It
          -- replaces the image's bundle, so it holds every authority to trust, that one included.
          , trustedCa : Optional Text
          -- Mount the pod's own service-account token, where Kubernetes clients look for it: for an
          -- agent that reads the Kubernetes API (the kube observer). Off otherwise.
          , serviceAccountToken : Bool
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
        , encrypts = False
        , custody = True
        , extraCerts = [] : List ExtraCert
        , scratch = [] : List Scratch
        , configFiles = [] : List ConfigFiles
        , trustedCa = None Text
        , serviceAccountToken = False
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

--| Whether this pod's keys are the custody daemon's: the platform has the daemon, and the agent has not opted out.
let custodyOn
    : Platform.Type → BuilderAgent.Type → Bool
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        merge
          { Some = λ(_ : Text) → agent.custody, None = False }
          platform.custodyImage

--| What the pod is given in place of a key file when its keys are the daemon's: where to sign.
let custodySocketEnv
    : Platform.Type → BuilderAgent.Type → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        if    custodyOn platform agent
        then  [ envValue "POLAR_CUSTODY_SOCKET" C.custody.socket ]
        else  [] : List kubernetes.EnvVar.Type

--| The variables every Cassini client reads. The key's is left out when the key is the daemon's: a client reads
--| a key file or the daemon's socket, exactly one.
let tlsEnv
    : Platform.Type → Bool → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
      λ(keyFile : Bool) →
          [ envValue "TLS_CA_CERT" C.certPaths.ca
          , envValue "TLS_CLIENT_CERT" C.certPaths.cert
          ]
        # ( if    keyFile
            then  [ envValue "TLS_CLIENT_KEY" C.certPaths.key ]
            else  [] : List kubernetes.EnvVar.Type
          )
        # [ envValue "BROKER_ADDR" platform.cassiniAddr
          , envValue "CASSINI_SERVER_NAME" platform.cassiniServerName
          ]

--| What the deployment provides, which the agent's builder compares with what its code requires and exits
--| non-zero on a mismatch (the fork's catalog derives it from `needs`; here it is `provides`). Always set: an
--| agent that requires nothing is told it is given nothing.
let capabilitiesEnv
    : BuilderAgent.Type → List kubernetes.EnvVar.Type
    = λ(agent : BuilderAgent.Type) →
        [ envValue "POLAR_CAPABILITIES" (Prelude.Text.concatSep "," agent.provides) ]

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
            then  tlsEnv platform (if custodyOn platform agent then False else True)
            else  [] : List kubernetes.EnvVar.Type
          )
        # custodySocketEnv platform agent
        # healthEnv platform agent
        # capabilitiesEnv agent

--| The CA polar-init verifies what it is issued against, when the platform pins one.
let pinnedCaEnv
    : Platform.Type → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
        merge
          { Some =
              λ(_ : Text) → [ envValue "POLAR_INIT_CA_CERT" C.pinnedCa.cert ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          platform.pinnedCa

--| The environment that says where polar-init gets certificates. On the credentials agent's path it also says
--| whose session offers to believe, which polar-init requires there (with the CA, `pinnedCaEnv`).
let issuerEnv
    : Platform.Type → Issuer → List kubernetes.EnvVar.Type
    = λ(platform : Platform.Type) →
      λ(issuer : Issuer) →
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
                , envValue
                    "POLAR_INIT_CREDENTIALS_AGENT_IDENTITY"
                    platform.credentialsAgentIdentity
                ]
          }
          issuer

--| Since the custody work polar-init no longer connects to the broker, so it is given none of the Cassini client's
--| variables, and has no escrow bootstrap (nor the service-account token it needed) of its own: an agent that
--| encrypts only asks, and the daemon's custodian fetches the key.
let escrowEnv
    : BuilderAgent.Type → List kubernetes.EnvVar.Type
    = λ(agent : BuilderAgent.Type) →
        if    agent.encrypts
        then  [ envValue "POLAR_INIT_ESCROW_BOOTSTRAP" "true" ]
        else  [] : List kubernetes.EnvVar.Type

--| `--cert` says who makes the certificate's key in its third field: polar-init, of an algorithm, or `custody`, the
--| pod's daemon.
let certArgumentWith
    : Text → CertKind → Text → Text
    = λ(algorithm : Text) →
      λ(kind : CertKind) →
      λ(dir : Text) →
        merge
          { Client = "client:${dir}:${algorithm}"
          , Server = λ(san : Text) → "server:${dir}:${algorithm}:${san}"
          }
          kind

let certArgumentIn
    : CertKind → Text → Text
    = certArgumentWith "ecdsa-p256"

let keyAlgorithm
    : Platform.Type → BuilderAgent.Type → Text
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        if custodyOn platform agent then "custody" else "ecdsa-p256"

let certArgument
    : CertKind → Text
    = λ(kind : CertKind) → certArgumentIn kind C.certDir

let extraCertArguments
    : Text → List ExtraCert → List Text
    = λ(algorithm : Text) →
      λ(certs : List ExtraCert) →
        Prelude.List.concatMap
          ExtraCert
          Text
          (λ(c : ExtraCert) → [ "--cert", certArgumentWith algorithm c.kind c.dir ])
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

let configFilesVolume
    : ConfigFiles → kubernetes.Volume.Type
    = λ(f : ConfigFiles) →
        kubernetes.Volume::{
        , name = f.configMap
        , configMap = Some kubernetes.ConfigMapVolumeSource::{ name = Some f.configMap }
        }

--| The projected service-account token only `polar-init` sees, with the audience the issuer wants.
let tokenVolume
    : Platform.Type → kubernetes.Volume.Type
    = λ(platform : Platform.Type) →
        kubernetes.Volume::{
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

--| Where the certificate and key are written, and read.
let certVolume
    : kubernetes.Volume.Type
    = kubernetes.Volume::{
      , name = certVolumeName
      , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
      }

--| The pinned CA's volume and where it is mounted, in the containers that verify against it.
let pinnedCaVolume
    : Text → kubernetes.Volume.Type
    = λ(configMap : Text) →
        kubernetes.Volume::{
        , name = C.pinnedCa.volume
        , configMap = Some kubernetes.ConfigMapVolumeSource::{ name = Some configMap }
        }

let pinnedCaMount
    : kubernetes.VolumeMount.Type
    = kubernetes.VolumeMount::{
      , name = C.pinnedCa.volume
      , mountPath = C.pinnedCa.dir
      , readOnly = Some True
      }

--| The socket's volume: memory, shared by the daemon, polar-init and the workload, and gone with the pod.
let custodyVolume
    : kubernetes.Volume.Type
    = kubernetes.Volume::{
      , name = C.custody.volume
      , emptyDir = Some kubernetes.EmptyDirVolumeSource::{ medium = Some "Memory" }
      }

let custodyMount
    : kubernetes.VolumeMount.Type
    = kubernetes.VolumeMount::{ name = C.custody.volume, mountPath = C.custody.dir }

--| The custody daemon, as a native sidecar: an init container that runs for the pod's whole life, declared before
--| polar-init so that it starts first. Its own user, the pod's group (which it shares its socket with), and every
--| capability dropped but the one that lets it lock its memory. polar-init and the workload run as one user
--| (`agent.runAsUser`), the daemon's installer and its workload both.
let custodySidecar
    : Platform.Type → BuilderAgent.Type → kubernetes.Container.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        kubernetes.Container::{
        , name = C.custody.sidecar
        , image = platform.custodyImage
        , imagePullPolicy = platform.imagePullPolicy
        , restartPolicy = Some "Always"
        , env = Some
          [ envValue "POLAR_CUSTODY_SOCKET" C.custody.socket
          , envValue "POLAR_CUSTODY_CA_CERT" C.pinnedCa.cert
          , envValue "POLAR_CUSTODY_INSTALLER_UID" (Natural/show agent.runAsUser)
          , envValue "POLAR_CUSTODY_WORKLOAD_UIDS" (Natural/show agent.runAsUser)
          -- Where it watches, as its pod, for its own revocation.
          , envValue "BROKER_ADDR" platform.cassiniAddr
          , envValue "CASSINI_SERVER_NAME" platform.cassiniServerName
          ]
        , volumeMounts = Some [ custodyMount, pinnedCaMount ]
        , securityContext = Some kubernetes.SecurityContext::{
          , runAsUser = Some C.custody.daemonUid
          , runAsGroup = Some agent.runAsUser
          , runAsNonRoot = Some True
          , allowPrivilegeEscalation = Some False
          , readOnlyRootFilesystem = Some True
          , capabilities = Some kubernetes.Capabilities::{
            , drop = Some [ "ALL" ]
            , add = Some [ "IPC_LOCK" ]
            }
          }
        }

--| The init container that obtains the pod's certificate.
let polarInit
    : Platform.Type → BuilderAgent.Type → kubernetes.Container.Type
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        let algorithm = keyAlgorithm platform agent

        in  kubernetes.Container::{
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
                  # [ "--cert", certArgumentWith algorithm agent.cert C.certDir ]
                  # extraCertArguments algorithm agent.extraCerts
                )
            , env = Some
                (   issuerEnv
                      platform
                      ( merge
                          { Some = λ(own : Issuer) → own, None = platform.issuer }
                          agent.issuer
                      )
                  # [ envValue "POLAR_INIT_TOKEN_PATH" tokenPath ]
                  # pinnedCaEnv platform
                  # custodySocketEnv platform agent
                  # escrowEnv agent
                  # platform.polarInitEnv
                )
            , volumeMounts = Some
              (   [ kubernetes.VolumeMount::{
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
                # merge
                    { Some = λ(_ : Text) → [ pinnedCaMount ]
                    , None = [] : List kubernetes.VolumeMount.Type
                    }
                    platform.pinnedCa
                # ( if    custodyOn platform agent
                    then  [ custodyMount ]
                    else  [] : List kubernetes.VolumeMount.Type
                  )
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

let trustedCaVolumeName = "trusted-ca"

let trustedCaDir = "/etc/polar-trusted-ca"

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
            { automountServiceAccountToken =
                if    agent.serviceAccountToken
                then  Some True
                else  None Bool
            , readOnlyRootFilesystem = None Bool
            , runAsUser = agent.runAsUser
            -- The group owns the volumes, so a claim is writable by the user the agent runs as.
            , fsGroup = Some agent.runAsUser
            , imagePullPolicy = platform.imagePullPolicy
            , initContainers =
                  ( if    custodyOn platform agent
                    then  [ custodySidecar platform agent ]
                    else  [] : List kubernetes.Container.Type
                  )
                # [ polarInit platform agent ]
            , volumes =
                  [ tokenVolume platform, certVolume ]
                # merge
                    { Some = λ(configMap : Text) → [ pinnedCaVolume configMap ]
                    , None = [] : List kubernetes.Volume.Type
                    }
                    platform.pinnedCa
                # ( if    custodyOn platform agent
                    then  [ custodyVolume ]
                    else  [] : List kubernetes.Volume.Type
                  )
                # Prelude.List.map ExtraCert kubernetes.Volume.Type extraCertVolume agent.extraCerts
                # Prelude.List.map Scratch kubernetes.Volume.Type scratchVolume agent.scratch
                # Prelude.List.map ConfigFiles kubernetes.Volume.Type configFilesVolume agent.configFiles
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
                # merge
                    { Some =
                        λ(secretName : Text) →
                          [ kubernetes.Volume::{
                            , name = trustedCaVolumeName
                            , secret = Some kubernetes.SecretVolumeSource::{ secretName = Some secretName }
                            }
                          ]
                    , None = [] : List kubernetes.Volume.Type
                    }
                    agent.trustedCa
            , volumeMounts =
                  [ kubernetes.VolumeMount::{
                    , name = certVolumeName
                    , mountPath = C.certDir
                    , readOnly = Some True
                    }
                  ]
                # ( if    custodyOn platform agent
                    then  [ custodyMount ]
                    else  [] : List kubernetes.VolumeMount.Type
                  )
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
                # Prelude.List.map
                    ConfigFiles
                    kubernetes.VolumeMount.Type
                    ( λ(f : ConfigFiles) →
                        kubernetes.VolumeMount::{
                        , name = f.configMap
                        , mountPath = f.mountPath
                        , readOnly = Some True
                        }
                    )
                    agent.configFiles
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
                # merge
                    { Some =
                        λ(secretName : Text) →
                          [ kubernetes.VolumeMount::{
                            , name = trustedCaVolumeName
                            , mountPath = trustedCaDir
                            , readOnly = Some True
                            }
                          ]
                    , None = [] : List kubernetes.VolumeMount.Type
                    }
                    agent.trustedCa
            , env =
                  clientEnv platform agent
                # agent.settings
                # merge
                    { Some =
                        λ(secretName : Text) →
                          [ kubernetes.EnvVar::{
                            , name = "SSL_CERT_FILE"
                            , value = Some "${trustedCaDir}/ca-bundle.crt"
                            }
                          ]
                    , None = [] : List kubernetes.EnvVar.Type
                    }
                    agent.trustedCa
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

--| What is wrong with an agent under this platform, in plain sentences; empty is nothing. The rules are the fork's
--| catalog's (ACE infra/rules.nu: `escrow_problems`, `custody_cert_problems`, and the pinned CA polar-init
--| requires of an agent that gets its certificates through the credentials agent). Dhall cannot fail from inside
--| a function, so a deployment asserts this is empty for every agent it renders (see `tests/custody.dhall`).
let problems
    : Platform.Type → BuilderAgent.Type → List Text
    = λ(platform : Platform.Type) →
      λ(agent : BuilderAgent.Type) →
        let on = custodyOn platform agent

        let throughObserver =
              merge
                { CertIssuer = λ(_ : Text) → False
                , CredentialsAgent = λ(_ : Text) → True
                }
                ( merge
                    { Some = λ(own : Issuer) → own, None = platform.issuer }
                    agent.issuer
                )

        let noCa = merge { Some = λ(_ : Text) → False, None = True } platform.pinnedCa

        let off = if on then False else True

        in    ( if    agent.encrypts && off
                then  [ "${agent.name} encrypts, and its escrow key is fetched only by the custody daemon's custodian, which this pod does not have: give the platform a custodyImage, or leave the agent out" ]
                else  [] : List Text
              )
            # ( if    on && noCa
                then  [ "${agent.name} has the custody daemon, which judges certificates by the pinned CA: set Platform.pinnedCa" ]
                else  [] : List Text
              )
            # ( if    throughObserver && noCa
                then  [ "${agent.name} gets its certificates through the credentials agent, and polar-init requires the pinned CA there: set Platform.pinnedCa" ]
                else  [] : List Text
              )

--| What a pod that is not an agent needs to have a certificate of its own: a build's job. It is the same
--| `polar-init` and the same projected token, with no escrow key (nothing in a job is encrypted to one) and
--| nothing waited for. The pod must run under a ServiceAccount of its own, because that is the certificate's
--| identity and the broker allows one session per identity. build-orchestrator makes one per build.
--|
--| Plain values, not Kubernetes records: the consumer is build-orchestrator, which reads them from its configuration
--| and builds the pod, and the records are expensive for Dhall to carry about.
let JobEnv = { name : Text, value : Text }

let JobIdentity =
      { image : Text
      , imagePullPolicy : Optional Text
      , uid : Natural
      , args : List Text
      , env : List JobEnv
      , tokenVolume : Text
      , tokenDir : Text
      , tokenAudience : Text
      , tokenExpirySecs : Natural
      , certVolume : Text
      , certDir : Text
      -- The pinned CA's ConfigMap, volume and mount, when the platform pins one: polar-init requires it, on this
      -- path. The ConfigMap must be in the jobs' namespace.
      , pinnedCa : Optional { configMap : Text, volume : Text, dir : Text }
      -- What the main container is told: where its certificate is and where the broker is.
      , pipelineEnv : List JobEnv
      }

--| Where `polar-init` gets certificates, as plain values.
let issuerJobEnv
    : Platform.Type → Issuer → List JobEnv
    = λ(platform : Platform.Type) →
      λ(issuer : Issuer) →
        merge
          { CertIssuer =
              λ(url : Text) →
                [ { name = "POLAR_INIT_ISSUER", value = "cert-issuer" }
                , { name = "POLAR_INIT_CERT_ISSUER_URL", value = url }
                ]
          , CredentialsAgent =
              λ(url : Text) →
                [ { name = "POLAR_INIT_ISSUER", value = "credentials-agent" }
                , { name = "POLAR_INIT_OBSERVER_URL", value = url }
                , { name = "POLAR_INIT_CREDENTIALS_AGENT_IDENTITY"
                  , value = platform.credentialsAgentIdentity
                  }
                ]
          }
          issuer

--| `extraEnv` is for what a deployment's `polar-init` needs besides (the platform's own `polarInitEnv` is
--| made of Kubernetes records, which are too costly for Dhall to carry through a function; say it again here
--| in plain values).
let makeJobIdentity
    : Platform.Type → List JobEnv → JobIdentity
    = λ(platform : Platform.Type) →
      λ(extraEnv : List JobEnv) →
        { image = platform.polarInitImage
        , imagePullPolicy = platform.imagePullPolicy
        , uid = 65532
        , args = [ "--cert", certArgument CertKind.Client ]
        , env =
              issuerJobEnv platform platform.issuer
            # [ { name = "POLAR_INIT_TOKEN_PATH", value = tokenPath } ]
            # merge
                { Some =
                    λ(_ : Text) →
                      [ { name = "POLAR_INIT_CA_CERT", value = C.pinnedCa.cert } ]
                , None = [] : List JobEnv
                }
                platform.pinnedCa
            # extraEnv
        , tokenVolume = tokenVolumeName
        , tokenDir
        , tokenAudience = platform.audience
        , tokenExpirySecs = platform.tokenExpirySecs
        , certVolume = certVolumeName
        , certDir = C.certDir
        , pinnedCa =
            merge
              { Some =
                  λ(configMap : Text) →
                    Some { configMap, volume = C.pinnedCa.volume, dir = C.pinnedCa.dir }
              , None = None { configMap : Text, volume : Text, dir : Text }
              }
              platform.pinnedCa
        , pipelineEnv =
          [ { name = "TLS_CA_CERT", value = C.certPaths.ca }
          , { name = "TLS_CLIENT_CERT", value = C.certPaths.cert }
          , { name = "TLS_CLIENT_KEY", value = C.certPaths.key }
          , { name = "BROKER_ADDR", value = platform.cassiniAddr }
          , { name = "CASSINI_SERVER_NAME", value = platform.cassiniServerName }
          ]
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
    , JobIdentity
    , JobEnv
    , makeJobIdentity
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
    , custodyOn
    , custodySidecar
    , problems
    , makeService
    , makeServiceFor
    , makePvc
    , makeServiceAccount
    , agentLabel
    , apiVersion
    , kind
    , optionalList
    }
