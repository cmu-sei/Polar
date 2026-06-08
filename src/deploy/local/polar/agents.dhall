{-
  local/agents.dhall — agent deployments for local cluster development.

  Depends on cert-issuer.dhall and cassini.dhall already being applied
  and healthy. Every agent pod runs polar-nu-init as an init container
  to obtain its mTLS client cert before starting.

  Deployment order:
    1. kubectl apply -f cert-issuer.dhall && kubectl rollout status deploy/cert-issuer -n polar
    2. kubectl apply -f cassini.dhall     && kubectl rollout status deploy/cassini -n polar
    3. kubectl apply -f agents.dhall
-}

let Polar      = ../../types/package.dhall
let C          = ../../types/lib-constants.dhall
let kubernetes = ../../types/kubernetes.dhall
let values     = ../values.dhall
let Agent      = Polar.agents
let functions  = Polar.functions

-- -------------------------------------------------------------------------
-- Deployment-local constants
-- These are not architectural constants (they don't belong in lib-constants)
-- and are not environment-specific values (they don't belong in values.dhall).
-- They are implementation details of how this deployment wires pods together.
-- -------------------------------------------------------------------------

let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let nuInitImage = "polar-nu-init:${commitSha}"

let saTokenVolumeName   = C.saTokenVolumeName
let certVolumeName       = C.certVolumeName
let initScriptVolumeName = C.initScriptVolumeName
let certTokenExpiry      =  C.certTokenExpiry
-- ConfigMap name for the nu init script, shared across all agent pods.
let initScriptConfigMapName = C.initScriptConfigMapName

-- Secret names for agent-specific credentials.
let gitObserverSecretName = "git-observer-secret"

-- OCI registry secret wired into the provenance resolver pod.
let ociRegistrySecretName  = "oci-registry-auth"
let ociRegistrySecretValue = env:DOCKER_AUTH_JSON as Text ? "someJson"

-- Well-known deployment/container names for linker and resolver.
-- These match the names the graph uses to identify these agents.
let artifactLinkerName     = "artifact-linker"
let provenanceLinkerName   = "provenance-linker"
let provenanceResolverName = "provenance-resolver"
let registryResolverName   = "oci-registry-resolver"

-- Security context applied to every container. Drop all capabilities,
-- run as non-root UID/GID 1000.
let dropAllCapSecurityContext =
      kubernetes.SecurityContext::{
      , runAsGroup  = Some 1000
      , runAsNonRoot = Some True
      , runAsUser   = Some 1000
      , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
      }

-- Istio sidecar injection annotation — applied to deployments that must
-- not have the sidecar (linker, resolver) to avoid mTLS double-wrapping.
let rejectSidecarAnnotation =
      { mapKey = "sidecar.istio.io/inject", mapValue = "false" }

-- Secret key selectors for credentials mounted via env.
let gitlabSecretKey = "token"

let graphSecretKeySelector =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key  = "secret"
      }

-- commonClientEnv: the five env vars every agent needs to connect to Cassini.
-- Derived entirely from lib-constants — no env var reads, no deployment state.
let commonClientEnv =
      [ kubernetes.EnvVar::{ name = "TLS_CA_CERT",          value = Some C.certPaths.ca }
      , kubernetes.EnvVar::{ name = "TLS_CLIENT_CERT",      value = Some C.certPaths.cert }
      , kubernetes.EnvVar::{ name = "TLS_CLIENT_KEY",       value = Some C.certPaths.key }
      , kubernetes.EnvVar::{ name = "BROKER_ADDR",          value = Some C.cassiniAddr }
      , kubernetes.EnvVar::{ name = "CASSINI_SERVER_NAME",  value = Some C.cassiniServerName }
      ]

-- -------------------------------------------------------------------------
-- Shared cert bootstrap volumes and mounts
-- -------------------------------------------------------------------------

let proxyCACert = values.proxyCACert

let baseCertVolumes =
      functions.makeCertVolumes
        saTokenVolumeName
        certVolumeName
        values.cassini.certClient.audience
        certTokenExpiry

let baseVolumes = baseCertVolumes # functions.ProxyVolume proxyCACert

let certMount =
      functions.makeAgentCertMount
        certVolumeName
        C.certDir

let baseMounts = certMount # functions.ProxyMount proxyCACert

let envVars =
      [ kubernetes.EnvVar::{ name = "RUST_LOG", value = Some "trace" } ]
      # commonClientEnv

-- Init script ConfigMap volume — same script mounted into every agent pod.
let initScriptVolume =
      kubernetes.Volume::{
      , name      = initScriptVolumeName
      , configMap = Some kubernetes.ConfigMapVolumeSource::{
        , name = Some initScriptConfigMapName
        }
      }

let agentVolumes = baseVolumes # [ initScriptVolume ]

-- -------------------------------------------------------------------------
-- Neo4j volumes, mounts, and env
-- -------------------------------------------------------------------------

let neo4jCAVolume =
      [ kubernetes.Volume::{
        , name   = "neo4j-bolt-ca"
        , secret = Some kubernetes.SecretVolumeSource::{
          , secretName = Some "neo4j-bolt-ca"
          }
        }
      ]

let neo4jCAVolumeMount =
      [ kubernetes.VolumeMount::{
        , name      = "neo4j-bolt-ca"
        , mountPath = "/etc/neo4j-ca"
        , readOnly  = Some True
        }
      ]

let neo4jBoltCASecret =
      functions.makeOpaqueSecret
        "neo4j-bolt-ca"
        "ca.pem"
        (env:NEO4J_TLS_CA_CERT_CONTENT as Text)

let neo4jEnvVars =
      [ kubernetes.EnvVar::{ name = "GRAPH_ENDPOINT", value = Some values.neo4jBoltAddr }
      , kubernetes.EnvVar::{ name = "GRAPH_DB",       value = Some values.neo4j.hostName }
      , kubernetes.EnvVar::{ name = "GRAPH_USER",     value = Some "neo4j" }
      , kubernetes.EnvVar::{
        , name      = "GRAPH_PASSWORD"
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some kubernetes.SecretKeySelector::{
            , name = Some "polar-graph-pw"
            , key  = "secret"
            }
          }
        }
      , kubernetes.EnvVar::{ name = "GRAPH_CA_CERT", value = Some C.certPaths.ca }
      ]

-- -------------------------------------------------------------------------
-- Init container factory
-- -------------------------------------------------------------------------

let makeCertInit =
      \(cfg : Polar.agents.CertClientConfig.Type) ->
        ( functions.makeNuInitContainer
            nuInitImage
            cfg
            saTokenVolumeName
            certVolumeName
            initScriptVolumeName
            dropAllCapSecurityContext
        ) // { imagePullPolicy = Some values.imagePullPolicy }

-- -------------------------------------------------------------------------
-- Secrets
-- -------------------------------------------------------------------------

let gitlabSecret =
      ( functions.makeOpaqueSecret
          "gitlab-secret"
          gitlabSecretKey
          (env:GITLAB_TOKEN as Text)
      ) // { immutable = Some True }

let gitObserverSecret =
      ( functions.makeOpaqueSecret
          gitObserverSecretName
          "git.json"
          values.gitObserver.config
      ) // { immutable = Some True }

let resolverSecret =
      ( functions.makeOpaqueSecret
          ociRegistrySecretName
          ociRegistrySecretName
          ociRegistrySecretValue
      ) // { immutable = Some True }

let kubeAgentServiceAccountToken =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind       = "Secret"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some values.kubeObserver.secretName
        , namespace = Some C.polarNamespace
        , annotations = Some
          [ { mapKey   = "kubernetes.io/service-account.name"
            , mapValue = values.kubeObserver.serviceAccountName
            }
          ]
        }
      , type = Some "kubernetes.io/service-account-token"
      }


-- -------------------------------------------------------------------------
-- Service accounts
-- -------------------------------------------------------------------------

let mkServiceAccount =
      \(name : Text) ->
        kubernetes.ServiceAccount::{
        , apiVersion = "v1"
        , kind       = "ServiceAccount"
        , metadata   = kubernetes.ObjectMeta::{
          , name      = Some name
          , namespace = Some C.polarNamespace
          }
        , automountServiceAccountToken = Some False
        }

let gitlabObserverSA = mkServiceAccount "gitlab-observer-sa"
let gitlabConsumerSA = mkServiceAccount "gitlab-consumer-sa"
let gitObserverSA    = mkServiceAccount "git-observer-sa"
let gitConsumerSA    = mkServiceAccount "git-consumer-sa"
let gitSchedulerSA   = mkServiceAccount "git-scheduler-sa"
let resolverSA       = mkServiceAccount "resolver-sa"
let buildProcessorSA = mkServiceAccount "build-processor-sa"
let kubeConsumerSA   = mkServiceAccount "kube-consumer-sa"

-- -------------------------------------------------------------------------
-- RBAC (kube observer)
-- -------------------------------------------------------------------------

let kubeAgentClusterRole =
      kubernetes.ClusterRole::{
      , apiVersion = "rbac.authorization.k8s.io/v1"
      , kind       = "ClusterRole"
      , metadata   = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }
      , rules      = Some
        [ kubernetes.PolicyRule::{
          , apiGroups = Some [ "*" ]
          , resources = Some [ "*" ]
          , verbs     = [ "get", "list", "watch" ]
          }
        ]
      }

let kubeAgentServiceAccount =
      kubernetes.ServiceAccount::{
      , apiVersion = "v1"
      , kind       = "ServiceAccount"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some values.kubeObserver.serviceAccountName
        , namespace = Some C.polarNamespace
        }
      , automountServiceAccountToken = Some False
      }

let kubeAgentRoleBinding =
      kubernetes.RoleBinding::{
      , apiVersion = "rbac.authorization.k8s.io/v1"
      , kind       = "ClusterRoleBinding"
      , metadata   = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }
      , roleRef    = kubernetes.RoleRef::{
        , apiGroup = "rbac.authorization.k8s.io"
        , kind     = "ClusterRole"
        , name     = "kube-observer-read"
        }
      , subjects = Some
        [ kubernetes.Subject::{
          , kind      = "ServiceAccount"
          , name      = values.kubeObserver.serviceAccountName
          , namespace = Some C.polarNamespace
          }
        ]
      }

-- -------------------------------------------------------------------------
-- Sidecar rejection helper
-- -------------------------------------------------------------------------

let withRejectSidecar =
      \(d : kubernetes.Deployment.Type) ->
        d // { metadata = d.metadata // { annotations = Some [ rejectSidecarAnnotation ] } }

-- -------------------------------------------------------------------------
-- GitLab agents
-- -------------------------------------------------------------------------

let gitlabObserverEnv =
      envVars
      # functions.ProxyEnv proxyCACert
      # [ kubernetes.EnvVar::{ name = "OBSERVER_BASE_INTERVAL", value = Some "30" }
        , kubernetes.EnvVar::{ name = "GITLAB_ENDPOINT",        value = Some values.gitlabObserver.endpoint }
        , kubernetes.EnvVar::{
          , name      = "GITLAB_TOKEN"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some kubernetes.SecretKeySelector::{
              , name = Some "gitlab-secret"
              , key  = gitlabSecretKey
              }
            }
          }
        ]

let gitlabConsumerEnv =
      envVars
      # functions.makeGraphEnv
          values.neo4jBoltAddr
          values.gitlabConsumer.graph
          graphSecretKeySelector
          (Some "/etc/neo4j-ca/ca.pem")

let gitlabAgentDeployment =
      functions.makeDeployment
        "gitlab-agents"
        kubernetes.PodSpec::{
        , serviceAccountName = Some "gitlab-observer-sa"
        , initContainers     = Some [ makeCertInit values.gitlabObserver.certClient ]
        , containers =
          [ kubernetes.Container::{
            , name            = values.gitlabObserver.name
            , image           = Some values.gitlabObserver.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some dropAllCapSecurityContext
            , env             = Some gitlabObserverEnv
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          , kubernetes.Container::{
            , name            = values.gitlabConsumer.name
            , image           = Some values.gitlabConsumer.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some dropAllCapSecurityContext
            , env             = Some gitlabConsumerEnv
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some (agentVolumes # neo4jCAVolume)
        }

-- -------------------------------------------------------------------------
-- Git agents
-- -------------------------------------------------------------------------

let gitObserverEnv =
      envVars
      # [ kubernetes.EnvVar::{ name = "GIT_AGENT_CONFIG", value = Some "git.json" } ]

let gitAgentDeployment =
      functions.makeDeployment
        "git-agents"
        kubernetes.PodSpec::{
        , serviceAccountName = Some "git-observer-sa"
        , initContainers     = Some [ makeCertInit values.gitObserver.certClient ]
        , containers =
          [ kubernetes.Container::{
            , name            = values.gitObserver.name
            , image           = Some values.gitObserver.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some dropAllCapSecurityContext
            , env             = Some gitObserverEnv
            , volumeMounts    = Some
                ( baseMounts
                  # [ kubernetes.VolumeMount::{
                      , name      = gitObserverSecretName
                      , mountPath = "/etc/git-observer"
                      }
                    ]
                )
            }
          , kubernetes.Container::{
            , name            = values.gitConsumer.name
            , image           = Some values.gitConsumer.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some dropAllCapSecurityContext
            , env             = Some
                ( envVars
                  # functions.makeGraphEnv
                      values.neo4jBoltAddr
                      values.gitConsumer.graph
                      graphSecretKeySelector
                      (Some "/etc/neo4j-ca/ca.pem")
                )
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          , kubernetes.Container::{
            , name            = values.gitScheduler.name
            , image           = Some values.gitScheduler.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some dropAllCapSecurityContext
            , env             = Some
                ( envVars
                  # functions.makeGraphEnv
                      values.neo4jBoltAddr
                      values.gitScheduler.graph
                      graphSecretKeySelector
                      (Some "/etc/neo4j-ca/ca.pem")
                )
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some
            ( agentVolumes
              # neo4jCAVolume
              # [ kubernetes.Volume::{
                  , name   = gitObserverSecretName
                  , secret = Some kubernetes.SecretVolumeSource::{
                    , secretName = Some gitObserverSecretName
                    }
                  }
                ]
            )
        }

-- -------------------------------------------------------------------------
-- Kube agents
-- -------------------------------------------------------------------------

let kubeAgentVolumes =
      agentVolumes
      # [ kubernetes.Volume::{
          , name   = values.kubeObserver.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some values.kubeObserver.secretName
            }
          }
        ]
      # neo4jCAVolume

let kubeObserverEnv =
      commonClientEnv
      # [ kubernetes.EnvVar::{
          , name      = "KUBE_TOKEN"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some kubernetes.SecretKeySelector::{
              , name = Some values.kubeObserver.secretName
              , key  = "token"
              }
            }
          }
        ]

let kubeConsumerEnv = envVars # neo4jEnvVars

let kubeObserverVolumeMounts =
      baseMounts
      # [ kubernetes.VolumeMount::{
          , name      = values.kubeObserver.secretName
          , mountPath = "/var/run/secrets/kubernetes.io/serviceaccount"
          }
        ]

let kubeAgentDeployment =
      functions.makeDeployment
        "kube-agents"
        kubernetes.PodSpec::{
        , serviceAccountName = Some values.kubeObserver.serviceAccountName
        , initContainers     = Some [ makeCertInit values.kubeObserver.certClient ]
        , containers =
          [ kubernetes.Container::{
            , name            = values.kubeObserver.name
            , image           = Some values.kubeObserver.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some dropAllCapSecurityContext
            , env             = Some kubeObserverEnv
            , volumeMounts    = Some kubeObserverVolumeMounts
            }
          , kubernetes.Container::{
            , name            = values.kubeConsumer.name
            , image           = Some values.kubeConsumer.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some dropAllCapSecurityContext
            , env             = Some kubeConsumerEnv
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some kubeAgentVolumes
        }

-- -------------------------------------------------------------------------
-- Build processor
-- -------------------------------------------------------------------------

let buildProcessorEnv =
      envVars
      # functions.makeGraphEnv
          values.neo4jBoltAddr
          values.buildProcessor.graph
          graphSecretKeySelector
          (Some "/etc/neo4j-ca/ca.pem")

let buildProcessorDeployment =
      functions.makeDeployment
        "build-processor"
        kubernetes.PodSpec::{
        , serviceAccountName = Some "build-processor-sa"
        , initContainers     = Some [ makeCertInit values.buildProcessor.certClient ]
        , containers =
          [ kubernetes.Container::{
            , name            = values.buildProcessor.name
            , image           = Some values.buildProcessor.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some dropAllCapSecurityContext
            , env             = Some buildProcessorEnv
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some (agentVolumes # neo4jCAVolume)
        }

-- -------------------------------------------------------------------------
-- resolver
-- -------------------------------------------------------------------------

let resolverVolumes =
      agentVolumes
      # [ kubernetes.Volume::{
          , name   = ociRegistrySecretName
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some ociRegistrySecretName
            , items      = Some
              [ kubernetes.KeyToPath::{ key = "oci-registry-auth", path = "config.json" } ]
            }
          }
        ]

let resolverVolumeMounts =
      baseMounts
      # [ kubernetes.VolumeMount::{
          , name      = ociRegistrySecretName
          , mountPath = "/home/polar/.docker/"
          }
        ]

let resolverDeployment =
      withRejectSidecar
        ( functions.makeDeployment
            registryResolverName
            kubernetes.PodSpec::{
            , serviceAccountName = Some "resolver-sa"
            , initContainers     = Some [ makeCertInit values.resolver.certClient ]
            , containers =
              [ kubernetes.Container::{
                , name            = provenanceResolverName
                , image           = Some values.resolver.image
                , imagePullPolicy = Some values.imagePullPolicy
                , securityContext = Some dropAllCapSecurityContext
                , env             = Some (envVars # functions.ProxyEnv proxyCACert)
                , volumeMounts    = Some resolverVolumeMounts
                }
              ]
            , volumes = Some resolverVolumes
            }
        )

-- -------------------------------------------------------------------------
-- Resource list
-- -------------------------------------------------------------------------

in  [ kubernetes.Resource.ClusterRole    kubeAgentClusterRole
    , kubernetes.Resource.Deployment     buildProcessorDeployment
    , kubernetes.Resource.Deployment     gitAgentDeployment
    , kubernetes.Resource.Deployment     gitlabAgentDeployment
    , kubernetes.Resource.Deployment     kubeAgentDeployment
    , kubernetes.Resource.Deployment     resolverDeployment
    , kubernetes.Resource.RoleBinding    kubeAgentRoleBinding
    , kubernetes.Resource.Secret         gitlabSecret
    , kubernetes.Resource.Secret         gitObserverSecret
    , kubernetes.Resource.Secret         kubeAgentServiceAccountToken
    , kubernetes.Resource.Secret         neo4jBoltCASecret
    , kubernetes.Resource.Secret         resolverSecret
    , kubernetes.Resource.ServiceAccount buildProcessorSA
    , kubernetes.Resource.ServiceAccount gitConsumerSA
    , kubernetes.Resource.ServiceAccount gitObserverSA
    , kubernetes.Resource.ServiceAccount gitSchedulerSA
    , kubernetes.Resource.ServiceAccount gitlabConsumerSA
    , kubernetes.Resource.ServiceAccount gitlabObserverSA
    , kubernetes.Resource.ServiceAccount kubeAgentServiceAccount
    , kubernetes.Resource.ServiceAccount kubeConsumerSA
    , kubernetes.Resource.ServiceAccount resolverSA
    ]
