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

let Agent      = ../../types/agents.dhall
let kubernetes = ../../types/kubernetes.dhall
let Constants  = ../../types/constants.dhall
let functions  = ../../types/functions.dhall
let values     = ../values.dhall

let proxyCACert  = values.proxyCACert
let nuInitImage  = "polar-nu-init:${Constants.commitSha}"

let scriptVolumeName = Constants.initScriptVolumeName

-- =============================================================================
-- Shared cert bootstrap volumes and mounts
-- =============================================================================

let baseCertVolumes =
      functions.makeCertVolumes
        Constants.saTokenVolumeName
        Constants.certVolumeName
        Constants.defaultCertClientConfig.audience
        Constants.certTokenExpiry

let baseVolumes = baseCertVolumes # functions.ProxyVolume proxyCACert

let certMount =
      functions.makeAgentCertMount
        Constants.certVolumeName
        Constants.certDir

let baseMounts = certMount # functions.ProxyMount proxyCACert

let envVars =
      [ kubernetes.EnvVar::{ name = "RUST_LOG", value = Some "debug" } ]
      # Constants.commonClientEnv

-- Init script ConfigMap volume — same script mounted into every agent pod.
-- The ConfigMap itself is emitted once in this file.
let initScriptVolume =
      kubernetes.Volume::{
      , name      = scriptVolumeName
      , configMap = Some kubernetes.ConfigMapVolumeSource::{
        , name        = Some Constants.initScriptConfigMapName
        }
      }

let agentVolumes = baseVolumes # [ initScriptVolume ]

-- =============================================================================
-- Neo4j
-- =============================================================================

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
      , kubernetes.EnvVar::{ name = "GRAPH_DB",       value = Some Constants.graphConfig.graphDB }
      , kubernetes.EnvVar::{ name = "GRAPH_USER",     value = Some Constants.graphConfig.graphUsername }
      , kubernetes.EnvVar::{
        , name      = "GRAPH_PASSWORD"
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some kubernetes.SecretKeySelector::{
            , name = Some "polar-graph-pw"
            , key  = Constants.neo4jSecret.key
            }
          }
        }
      , kubernetes.EnvVar::{ name = "GRAPH_CA_CERT", value = Some "/etc/neo4j-ca/ca.pem" }
      ]

-- =============================================================================
-- Init container factory
-- Every agent gets the same nu-init container with its own certClient config.
-- For testing, we explcitly never pull images.
-- =============================================================================

let makeCertInit =
      \(cfg : Agent.CertClientConfig) ->
      (        functions.makeNuInitContainer
        nuInitImage
        cfg
        Constants.saTokenVolumeName
        Constants.certVolumeName
        scriptVolumeName) // { imagePullPolicy = Some "Never" }

-- =============================================================================
-- Init script ConfigMap — emitted once, mounted into every agent pod
-- =============================================================================

let agentInitScriptConfigMap =
      functions.makeNuInitScript
        Constants.initScriptConfigMapName
        Constants.polarInitScript

-- =============================================================================
-- Secrets
-- =============================================================================

let graphSecret =
      functions.makeOpaqueSecret
        "polar-graph-pw"
        Constants.neo4jSecret.key
        (env:GRAPH_PASSWORD as Text)

let gitlabSecret =
      ( functions.makeOpaqueSecret
          "gitlab-secret"
          Constants.gitlabSecretKeySelector.key
          (env:GITLAB_TOKEN as Text)
      ) // { immutable = Some True }

let gitObserverSecret =
      ( functions.makeOpaqueSecret
          Constants.gitObserverSecretName
          "git.json"
          values.gitObserver.config
      ) // { immutable = Some True }

let resolverSecret =
      ( functions.makeOpaqueSecret
          Constants.OciRegistrySecret.name
          Constants.OciRegistrySecret.name
          Constants.OciRegistrySecret.value
      ) // { immutable = Some True }

let kubeAgentServiceAccountToken =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind       = "Secret"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some values.kubeObserver.secretName
        , namespace = Some Constants.PolarNamespace
        , annotations = Some
          [ { mapKey   = "kubernetes.io/service-account.name"
            , mapValue = values.kubeObserver.serviceAccountName
            }
          ]
        }
      , type = Some "kubernetes.io/service-account-token"
      }

let buildOrchestratorServiceAccountToken =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind       = "Secret"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some values.buildOrchestrator.secretName
        , namespace = Some Constants.PolarNamespace
        , annotations = Some
          [ { mapKey   = "kubernetes.io/service-account.name"
            , mapValue = values.buildOrchestrator.serviceAccountName
            }
          ]
        }
      , type = Some "kubernetes.io/service-account-token"
      }

-- =============================================================================
-- Service accounts
-- =============================================================================

let mkServiceAccount =
      \(name : Text) ->
        kubernetes.ServiceAccount::{
        , apiVersion = "v1"
        , kind       = "ServiceAccount"
        , metadata   = kubernetes.ObjectMeta::{
          , name      = Some name
          , namespace = Some Constants.PolarNamespace
          }
        , automountServiceAccountToken = Some False
        }

let gitlabObserverSA = mkServiceAccount "gitlab-observer-sa"
let gitlabConsumerSA = mkServiceAccount "gitlab-consumer-sa"
let gitObserverSA    = mkServiceAccount "git-observer-sa"
let gitConsumerSA    = mkServiceAccount "git-consumer-sa"
let gitSchedulerSA   = mkServiceAccount "git-scheduler-sa"
let linkerSA         = mkServiceAccount "linker-sa"
let resolverSA       = mkServiceAccount "resolver-sa"
let buildProcessorSA = mkServiceAccount "build-processor-sa"
let kubeConsumerSA   = mkServiceAccount "kube-consumer-sa"

-- =============================================================================
-- RBAC (kube observer)
-- =============================================================================

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
        , namespace = Some Constants.PolarNamespace
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
          , namespace = Some Constants.PolarNamespace
          }
        ]
      }

-- =============================================================================
-- RBAC (build orchestrator)
-- =============================================================================

--let buildOrchestratorClusterRoleName = "build-orchestrator-read"

--let buildOrchestratorClusterRole =
--      kubernetes.ClusterRole::{
--      , apiVersion = "rbac.authorization.k8s.io/v1"
--      , kind       = "ClusterRole"
--      , metadata   = kubernetes.ObjectMeta::{ name = Some buildOrchestratorClusterRoleName }
--      , rules      = Some
--        [ kubernetes.PolicyRule::{
--          , apiGroups = Some [ "*" ]
--          , resources = Some [ "*" ]
--          , verbs     = [ "get", "list", "watch" ]
--          }
--        ]
--      }

--let buildOrchestratorServiceAccount =
--      kubernetes.ServiceAccount::{
--      , apiVersion = "v1"
--      , kind       = "ServiceAccount"
--      , metadata   = kubernetes.ObjectMeta::{
--        , name      = Some values.buildOrchestrator.serviceAccountName
--        , namespace = Some Constants.PolarNamespace
--        }
--      , automountServiceAccountToken = Some False
--      }

--let buildOrchestratorRoleBinding =
--      kubernetes.RoleBinding::{
--      , apiVersion = "rbac.authorization.k8s.io/v1"
--      , kind       = "ClusterRoleBinding"
--      , metadata   = kubernetes.ObjectMeta::{ name = Some buildOrchestratorClusterRoleName }
--      , roleRef    = kubernetes.RoleRef::{
--        , apiGroup = "rbac.authorization.k8s.io"
--        , kind     = "ClusterRole"
--        , name     = buildOrchestratorClusterRoleName
--        }
--      , subjects = Some
--        [ kubernetes.Subject::{
--          , kind      = "ServiceAccount"
--          , name      = values.buildOrchestrator.serviceAccountName
--          , namespace = Some Constants.PolarNamespace
--          }
--        ]
--      }

-- =============================================================================
-- Sidecar rejection helper
-- =============================================================================

let withRejectSidecar =
      \(d : kubernetes.Deployment.Type) ->
        d // { metadata = d.metadata // { annotations = Some [ Constants.RejectSidecarAnnotation ] } }

-- =============================================================================
-- GitLab agents
-- =============================================================================

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
              , key  = "token"
              }
            }
          }
        ]

let gitlabConsumerEnv =
      envVars
      # functions.makeGraphEnv
          values.neo4jBoltAddr
          values.gitlabConsumer.graph
          Constants.graphSecretKeySelector
          (Some "/etc/neo4j-ca/ca.pem")

let gitlabAgentDeployment =
      functions.makeDeployment
        "gitlab-agents"
        kubernetes.PodSpec::{
        , imagePullSecrets   = Some values.imagePullSecrets
        , serviceAccountName = Some "gitlab-observer-sa"
        , initContainers     = Some [ makeCertInit values.gitlabObserver.certClient ]
        , containers =
          [ kubernetes.Container::{
            , name            = values.gitlabObserver.name
            , image           = Some values.gitlabObserver.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env             = Some gitlabObserverEnv
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          , kubernetes.Container::{
            , name            = values.gitlabConsumer.name
            , image           = Some values.gitlabConsumer.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env             = Some gitlabConsumerEnv
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some (agentVolumes # neo4jCAVolume)
        }

-- =============================================================================
-- Git agents
-- =============================================================================

let gitObserverEnv =
      envVars
      # [ kubernetes.EnvVar::{ name = "GIT_AGENT_CONFIG", value = Some "git.json" } ]

let gitAgentDeployment =
      functions.makeDeployment
        "git-agents"
        kubernetes.PodSpec::{
        , imagePullSecrets   = Some values.imagePullSecrets
        , serviceAccountName = Some "git-observer-sa"
        , initContainers     = Some [ makeCertInit values.gitObserver.certClient ]
        , containers =
          [ kubernetes.Container::{
            , name            = values.gitObserver.name
            , image           = Some values.gitObserver.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env             = Some gitObserverEnv
            , volumeMounts    = Some
                ( baseMounts
                  # [ kubernetes.VolumeMount::{
                      , name      = Constants.gitObserverSecretName
                      , mountPath = "/etc/git-observer"
                      }
                    ]
                )
            }
          , kubernetes.Container::{
            , name            = values.gitConsumer.name
            , image           = Some values.gitConsumer.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env             = Some (envVars # functions.makeGraphEnv values.neo4jBoltAddr values.gitConsumer.graph Constants.graphSecretKeySelector (Some "/etc/neo4j-ca/ca.pem"))
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          , kubernetes.Container::{
            , name            = values.gitScheduler.name
            , image           = Some values.gitScheduler.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env             = Some (envVars # functions.makeGraphEnv values.neo4jBoltAddr values.gitScheduler.graph Constants.graphSecretKeySelector (Some "/etc/neo4j-ca/ca.pem"))
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some
            ( agentVolumes
              # neo4jCAVolume
              # [ kubernetes.Volume::{
                  , name   = Constants.gitObserverSecretName
                  , secret = Some kubernetes.SecretVolumeSource::{
                    , secretName = Some Constants.gitObserverSecretName
                    }
                  }
                ]
            )
        }

-- =============================================================================
-- Kube agents
-- =============================================================================

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
      Constants.commonClientEnv
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

let kubeConsumerEnv =
      envVars
      # functions.makeGraphEnv
          values.neo4jBoltAddr
          values.kubeConsumer.graph
          Constants.graphSecretKeySelector
          (Some "/etc/neo4j-ca/ca.pem")

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
        , imagePullSecrets   = Some values.imagePullSecrets
        , serviceAccountName = Some values.kubeObserver.serviceAccountName
        , initContainers     = Some [ makeCertInit values.kubeObserver.certClient ]
        , containers =
          [ kubernetes.Container::{
            , name            = values.kubeObserver.name
            , image           = Some values.kubeObserver.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env             = Some kubeObserverEnv
            , volumeMounts    = Some kubeObserverVolumeMounts
            }
          , kubernetes.Container::{
            , name            = values.kubeConsumer.name
            , image           = Some values.kubeConsumer.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env             = Some kubeConsumerEnv
            , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some kubeAgentVolumes
        }
        -- =============================================================================
        -- Build processor (build orchestrator deprecated, removed from local deploy)
        -- =============================================================================

        let buildProcessorEnv =
              envVars
              # functions.makeGraphEnv
                  values.neo4jBoltAddr
                  values.buildProcessor.graph
                  Constants.graphSecretKeySelector
                  (Some "/etc/neo4j-ca/ca.pem")

        let buildProcessorDeployment =
              functions.makeDeployment
                "build-processor"
                kubernetes.PodSpec::{
                , imagePullSecrets   = Some values.imagePullSecrets
                , serviceAccountName = Some "build-processor-sa"
                , initContainers     = Some [ makeCertInit values.buildProcessor.certClient ]
                , containers =
                  [ kubernetes.Container::{
                    , name            = values.buildProcessor.name
                    , image           = Some values.buildProcessor.image
                    , imagePullPolicy = Some values.imagePullPolicy
                    , securityContext = Some Constants.DropAllCapSecurityContext
                    , env             = Some buildProcessorEnv
                    , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
                    }
                  ]
                , volumes = Some (agentVolumes # neo4jCAVolume)
                }

-- =============================================================================
-- Linker and resolver
-- =============================================================================

let linkerDeployment =
      withRejectSidecar
        ( functions.makeDeployment
            Constants.ArtifactLinkerName
            kubernetes.PodSpec::{
            , imagePullSecrets   = Some values.imagePullSecrets
            , serviceAccountName = Some "linker-sa"
            , initContainers     = Some [ makeCertInit values.linker.certClient ]
            , containers =
              [ kubernetes.Container::{
                , name            = Constants.ProvenanceLinkerName
                , image           = Some values.linker.image
                , imagePullPolicy = Some values.imagePullPolicy
                , securityContext = Some Constants.DropAllCapSecurityContext
                , env             = Some (envVars # neo4jEnvVars)
                , volumeMounts    = Some (baseMounts # neo4jCAVolumeMount)
                }
              ]
            , volumes = Some (agentVolumes # neo4jCAVolume)
            }
        )

let resolverVolumes =
      agentVolumes
      # [ kubernetes.Volume::{
          , name   = Constants.OciRegistrySecret.name
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some Constants.OciRegistrySecret.name
            , items      = Some [ kubernetes.KeyToPath::{ key = "oci-registry-auth", path = "config.json" } ]
            }
          }
        ]

let resolverVolumeMounts =
      baseMounts
      # [ kubernetes.VolumeMount::{
          , name      = Constants.OciRegistrySecret.name
          , mountPath = "/home/polar/.docker/"
          }
        ]

let resolverDeployment =
      withRejectSidecar
        ( functions.makeDeployment
            Constants.RegistryResolverName
            kubernetes.PodSpec::{
            , imagePullSecrets   = Some values.imagePullSecrets
            , serviceAccountName = Some "resolver-sa"
            , initContainers     = Some [ makeCertInit values.resolver.certClient ]
            , containers =
              [ kubernetes.Container::{
                , name            = Constants.ProvenanceResolverName
                , image           = Some values.resolver.image
                , imagePullPolicy = Some values.imagePullPolicy
                , securityContext = Some Constants.DropAllCapSecurityContext
                , env             = Some (envVars # functions.ProxyEnv proxyCACert)
                , volumeMounts    = Some resolverVolumeMounts
                }
              ]
            , volumes = Some resolverVolumes
            }
        )

-- =============================================================================
-- Resource list
-- =============================================================================

in  [ kubernetes.Resource.ClusterRole    kubeAgentClusterRole
    , kubernetes.Resource.Deployment     buildProcessorDeployment
    , kubernetes.Resource.Deployment     gitAgentDeployment
    , kubernetes.Resource.Deployment     gitlabAgentDeployment
    , kubernetes.Resource.Deployment     kubeAgentDeployment
    , kubernetes.Resource.Deployment     linkerDeployment
    , kubernetes.Resource.Deployment     resolverDeployment
    , kubernetes.Resource.RoleBinding    kubeAgentRoleBinding
    , kubernetes.Resource.Secret         gitlabSecret
    , kubernetes.Resource.Secret         graphSecret
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
    , kubernetes.Resource.ServiceAccount linkerSA
    , kubernetes.Resource.ServiceAccount resolverSA
    , kubernetes.Resource.ConfigMap      agentInitScriptConfigMap
    ]
