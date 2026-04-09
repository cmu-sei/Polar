let Agent = ../../types/agents.dhall

let kubernetes = ../../types/kubernetes.dhall

let Constants = ../../types/constants.dhall

let functions = ../../types/functions.dhall

let values = ../values-active.dhall

let linker = values.linker

let resolver = values.resolver

-- Name of the secret containing a proxy ca certificate to be mounted into containers that need it.
let proxyCACert = values.proxyCACert

let volumes = Constants.commonVolumes # functions.ProxyVolume proxyCACert

let volumeMounts = [ values.cassiniTlsVolumeMount ] # functions.ProxyMount proxyCACert

let neo4jCAVolume =
      [ kubernetes.Volume::{
        , name = "neo4j-bolt-ca"
        , secret = Some kubernetes.SecretVolumeSource::{
          , secretName = Some "neo4j-bolt-ca"
          }
        }
      ]

let neo4jCAVolumeMount =
      [ kubernetes.VolumeMount::{
        , name = "neo4j-bolt-ca"
        , mountPath = "/etc/neo4j-ca"
        , readOnly = Some True
        }
      ]

let neo4jBoltCASecret =
      functions.makeOpaqueSecret
        "neo4j-bolt-ca"
        "ca.pem"
        (env:NEO4J_TLS_CA_CERT_CONTENT as Text)

-- Base env for all agents: RUST_LOG + mTLS client config
let envVars =
      [ kubernetes.EnvVar::{ name = "RUST_LOG", value = Some "debug" } ]
      # Constants.commonClientEnv

let neo4jEnvVars =
      [ kubernetes.EnvVar::{
        , name = "GRAPH_ENDPOINT"
        , value = Some values.neo4jBoltAddr
        }
      , kubernetes.EnvVar::{
        , name = "GRAPH_DB"
        , value = Some Constants.graphConfig.graphDB
        }
      , kubernetes.EnvVar::{
        , name = "GRAPH_USER"
        , value = Some Constants.graphConfig.graphUsername
        }
      , kubernetes.EnvVar::{
        , name = "GRAPH_PASSWORD"
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some kubernetes.SecretKeySelector::{
            , name = Some "polar-graph-pw"
            , key = Constants.neo4jSecret.key
            }
          }
        }
      , kubernetes.EnvVar::{
        , name = "GRAPH_CA_CERT"
        , value = Some "/etc/neo4j-ca/ca.pem"
        }
      ]

-- =============================================================================
-- Secrets
-- =============================================================================

let graphSecret =
      functions.makeOpaqueSecret
        "polar-graph-pw"
        Constants.neo4jSecret.key
        (env:GRAPH_PASSWORD as Text)

let gitlabSecret =
      (functions.makeOpaqueSecret
        "gitlab-secret"
        Constants.gitlabSecretKeySelector.key
        (env:GITLAB_TOKEN as Text))
      // { immutable = Some True }

let gitObserverSecret =
      (functions.makeOpaqueSecret
        Constants.gitObserverSecretName
        "git.json"
        values.gitObserver.config)
      // { immutable = Some True }

let resolverSecret =
      (functions.makeOpaqueSecret
        Constants.OciRegistrySecret.name
        Constants.OciRegistrySecret.name
        Constants.OciRegistrySecret.value)
      // { immutable = Some True }

let kubeAgentServiceAccountToken =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.kubeObserver.secretName
        , namespace = Some Constants.PolarNamespace
        , annotations = Some
          [ { mapKey = "kubernetes.io/service-account.name"
            , mapValue = values.kubeObserver.serviceAccountName
            }
          ]
        }
      , type = Some "kubernetes.io/service-account-token"
      }

let buildOrchestratorServiceAccountToken =
    kubernetes.Secret::{
    , apiVersion = "v1"
    , kind = "Secret"
    , metadata = kubernetes.ObjectMeta::{
        , name = Some values.buildOrchestrator.secretName
        , namespace = Some Constants.PolarNamespace
        , annotations = Some
        [ { mapKey = "kubernetes.io/service-account.name"
            , mapValue = values.buildOrchestrator.serviceAccountName
            }
        ]
        }
    , type = Some "kubernetes.io/service-account-token"
    }

-- =============================================================================
-- RBAC (kube observer)
-- =============================================================================

let kubeAgentClusterRole =
      kubernetes.ClusterRole::{
      , apiVersion = "rbac.authorization.k8s.io/v1"
      , kind = "ClusterRole"
      , metadata = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }
      , rules = Some
        [ kubernetes.PolicyRule::{
          , apiGroups = Some [ "*" ]
          , resources = Some [ "*" ]
          , verbs = [ "get", "list", "watch" ]
          }
        ]
      }

let kubeAgentServiceAccount =
      kubernetes.ServiceAccount::{
      , apiVersion = "v1"
      , kind = "ServiceAccount"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.kubeObserver.serviceAccountName
        , namespace = Some Constants.PolarNamespace
        }
      , automountServiceAccountToken = Some False
      }

let kubeAgentRoleBinding =
      kubernetes.RoleBinding::{
      , apiVersion = "rbac.authorization.k8s.io/v1"
      , kind = "ClusterRoleBinding"
      , metadata = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }
      , roleRef = kubernetes.RoleRef::{
        , apiGroup = ""
        , kind = "ClusterRole"
        , name = "kube-observer-read"
        }
      , subjects = Some
        [ kubernetes.Subject::{
          , kind = "ServiceAccount"
          , name = values.kubeObserver.serviceAccountName
          , namespace = Some Constants.PolarNamespace
          }
        ]
      }

-- =============================================================================
-- RBAC (build orchestrator)
-- =============================================================================

let buildOrchestratorClusterRoleName = "build-orchestrator-read"

let buildOrchestratorClusterRole =
    kubernetes.ClusterRole::{
    , apiVersion = "rbac.authorization.k8s.io/v1"
    , kind = "ClusterRole"
    , metadata = kubernetes.ObjectMeta::{ name = Some buildOrchestratorClusterRoleName }
    , rules = Some
        [ kubernetes.PolicyRule::{
        , apiGroups = Some [ "*" ]
        , resources = Some [ "*" ]
        , verbs = [ "get", "list", "watch" ]
        }
        ]
    }

let buildOrchestratorServiceAccount =
    kubernetes.ServiceAccount::{
    , apiVersion = "v1"
    , kind = "ServiceAccount"
    , metadata = kubernetes.ObjectMeta::{
        , name = Some values.buildOrchestrator.serviceAccountName
        , namespace = Some Constants.PolarNamespace
        }
    , automountServiceAccountToken = Some False
    }

let buildOrchestratorRoleBinding =
    kubernetes.RoleBinding::{
    , apiVersion = "rbac.authorization.k8s.io/v1"
    , kind = "ClusterRoleBinding"
    , metadata = kubernetes.ObjectMeta::{ name = Some buildOrchestratorClusterRoleName }
    , roleRef = kubernetes.RoleRef::{
        , apiGroup = ""
        , kind = "ClusterRole"
        , name = buildOrchestratorClusterRoleName
        }
    , subjects = Some
        [ kubernetes.Subject::{
        , kind = "ServiceAccount"
        , name = values.kubeObserver.serviceAccountName
        , namespace = Some Constants.PolarNamespace
        }
        ]
    }


-- =============================================================================
-- Gitlab agents deployment
-- =============================================================================

let gitlabObserverEnv =
        envVars
      # functions.ProxyEnv proxyCACert
      # [ kubernetes.EnvVar::{ name = "OBSERVER_BASE_INTERVAL", value = Some "30" }
        , kubernetes.EnvVar::{ name = "GITLAB_ENDPOINT", value = Some values.gitlabObserver.endpoint }
        , kubernetes.EnvVar::{
          , name = "GITLAB_TOKEN"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some kubernetes.SecretKeySelector::{
              , name = Some "gitlab-secret"
              , key = "token"
              }
            }
          }
        ]

let gitlabConsumerEnv =
      envVars # functions.makeGraphEnv values.neo4jBoltAddr values.gitlabConsumer.graph Constants.graphSecretKeySelector (Some "/etc/neo4j-ca/ca.pem")

let gitlabAgentDeployment =
      functions.makeDeployment
        "gitlab-agents"
        kubernetes.PodSpec::{
        , imagePullSecrets = Some values.imagePullSecrets
        , containers =
          [ kubernetes.Container::{
            , name = values.gitlabObserver.name
            , image = Some values.gitlabObserver.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env = Some gitlabObserverEnv
            , volumeMounts = Some (volumeMounts # neo4jCAVolumeMount)
            }
          , kubernetes.Container::{
            , name = values.gitlabConsumer.name
            , image = Some values.gitlabConsumer.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env = Some gitlabConsumerEnv
            , volumeMounts = Some (volumeMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some (Constants.commonVolumes # functions.ProxyVolume proxyCACert # neo4jCAVolume)
        }

-- =============================================================================
-- Git observer env (no graph access, config path only)
-- =============================================================================

let gitObserverEnv =
      envVars # [ kubernetes.EnvVar::{ name = "GIT_AGENT_CONFIG", value = Some "git.json" } ]

-- =============================================================================
-- Kube agent deployment
-- =============================================================================

let kubeAgentVolumes =
        volumes
      # [ kubernetes.Volume::{
          , name = values.kubeObserver.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some values.kubeObserver.secretName
            }
          }
        ]

let kubeObserverEnv =
      Constants.commonClientEnv
      # [ kubernetes.EnvVar::{
          , name = "KUBE_TOKEN"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some kubernetes.SecretKeySelector::{
              , name = Some values.kubeObserver.secretName
              , key = "token"
              }
            }
          }
        ]

let kubeConsumerEnv =
      Constants.commonClientEnv
      # functions.makeGraphEnv values.neo4jBoltAddr values.kubeConsumer.graph Constants.graphSecretKeySelector (Some "/etc/neo4j-ca/ca.pem")

let kubeObserverVolumeMounts =
        [ kubernetes.VolumeMount::{
          , name = values.kubeObserver.secretName
          , mountPath = "/var/run/secrets/kubernetes.io/serviceaccount"
          }
        ]
      # volumeMounts

let kubeAgentDeployment =
      functions.makeDeployment
        "kube-agent"
        kubernetes.PodSpec::{
        , imagePullSecrets = Some values.imagePullSecrets
        , serviceAccountName = Some values.kubeObserver.serviceAccountName
        , containers =
          [ kubernetes.Container::{
            , name = values.kubeObserver.name
            , image = Some values.kubeObserver.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env = Some kubeObserverEnv
            , volumeMounts = Some kubeObserverVolumeMounts
            }
          , kubernetes.Container::{
            , name = values.kubeConsumer.name
            , image = Some values.kubeConsumer.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env = Some kubeConsumerEnv
            , volumeMounts = Some (volumeMounts # neo4jCAVolumeMount)
            }
          ]
        , volumes = Some (kubeAgentVolumes # neo4jCAVolume)
        }


-- =============================================================================
-- Build Orchestrator agent deployment
-- =============================================================================

let buildOrchestratorConfigSecret =
      (functions.makeOpaqueSecret
        "build-orchestrator-config"
        "cyclops.yaml"
        values.buildOrchestrator.config)
      // { immutable = Some True }

let buildOrchestratorVolumes =
        volumes
        # [ kubernetes.Volume::{
            , name = values.buildOrchestrator.secretName
            , secret = Some kubernetes.SecretVolumeSource::{
              , secretName = Some values.buildOrchestrator.secretName
              }
            }
          , kubernetes.Volume::{
            , name = "build-orchestrator-config"
            , secret = Some kubernetes.SecretVolumeSource::{
              , secretName = Some "build-orchestrator-config"
              }
            }
          ]

let buildOrchestratorEnv =
        Constants.commonClientEnv
        # [ kubernetes.EnvVar::{
            , name = "KUBE_TOKEN"
            , valueFrom = Some kubernetes.EnvVarSource::{
              , secretKeyRef = Some kubernetes.SecretKeySelector::{
                  , name = Some values.buildOrchestrator.secretName
                  , key = "token"
                  }
              }
            }
          , kubernetes.EnvVar::{
            , name = "ORCHESTRATOR_CONFIG_FILE"
            , value = Some "/etc/cyclops/cyclops.yaml"
            }
          ]

let buildOrchestratorVolumeMounts =
        [ kubernetes.VolumeMount::{
            , name = values.buildOrchestrator.secretName
            , mountPath = "/var/run/secrets/kubernetes.io/serviceaccount"
            }
          , kubernetes.VolumeMount::{
            , name = "build-orchestrator-config"
            , mountPath = "/etc/cyclops"
            , readOnly = Some True
            }
          ]
        # volumeMounts

let buildProcessorEnv =
        Constants.commonClientEnv
        # functions.makeGraphEnv values.neo4jBoltAddr values.buildProcessor.graph Constants.graphSecretKeySelector (Some "/etc/neo4j-ca/ca.pem")
        # [ kubernetes.EnvVar::{
            , name = "ORCHESTRATOR_CONFIG_FILE"
            , value = Some "/etc/cyclops/cyclops.yaml"
            }
          ]

let buildDeployment =
        functions.makeDeployment
        "build-agents"
        kubernetes.PodSpec::{
        , imagePullSecrets = Some values.imagePullSecrets
        , serviceAccountName = Some values.buildOrchestrator.serviceAccountName
        , containers =
            [ kubernetes.Container::{
            , name = values.buildOrchestrator.name
            , image = Some values.buildOrchestrator.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env = Some buildOrchestratorEnv
            , volumeMounts = Some buildOrchestratorVolumeMounts
            }
            , kubernetes.Container::{
            , name = values.buildProcessor.name
            , image = Some values.buildProcessor.image
            , imagePullPolicy = Some values.imagePullPolicy
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env = Some buildProcessorEnv
            , volumeMounts = Some (volumeMounts # [ kubernetes.VolumeMount::{
              , name = "build-orchestrator-config"
              , mountPath = "/etc/cyclops"
              , readOnly = Some True
            } ] # neo4jCAVolumeMount)
            }
            ]
        , volumes = Some (buildOrchestratorVolumes # neo4jCAVolume)
        }

-- =============================================================================
-- Linker and resolver deployments (sidecar injection rejected via annotation)
-- =============================================================================

let withRejectSidecar =
      \(d : kubernetes.Deployment.Type) ->
        d // { metadata = d.metadata // { annotations = Some [ Constants.RejectSidecarAnnotation ] } }

let linkerEnv = envVars # neo4jEnvVars

let linkerDeployment =
      withRejectSidecar
        ( functions.makeDeployment
            Constants.ArtifactLinkerName
            kubernetes.PodSpec::{
            , imagePullSecrets = Some values.imagePullSecrets
            , containers =
              [ kubernetes.Container::{
                , name = Constants.ProvenanceLinkerName
                , image = Some linker.image
                , imagePullPolicy = Some values.imagePullPolicy
                , securityContext = Some Constants.DropAllCapSecurityContext
                , env = Some linkerEnv
                , volumeMounts = Some (volumeMounts # neo4jCAVolumeMount)
                }
              ]
            , volumes = Some (volumes # neo4jCAVolume)
            }
        )

let resolverEnv = envVars # functions.ProxyEnv proxyCACert

let resolverVolumes =
        volumes
      # [ kubernetes.Volume::{
          , name = Constants.OciRegistrySecret.name
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some Constants.OciRegistrySecret.name
            , items = Some
              [ kubernetes.KeyToPath::{ key = "oci-registry-auth", path = "config.json" } ]
            }
          }
        ]

let resolverVolumeMounts =
        [ kubernetes.VolumeMount::{
          , name = Constants.OciRegistrySecret.name
          , mountPath = "/home/polar/.docker/"
          }
        ]
      # volumeMounts

let resolverDeployment =
      withRejectSidecar
        ( functions.makeDeployment
            Constants.RegistryResolverName
            kubernetes.PodSpec::{
            , imagePullSecrets = Some values.imagePullSecrets
            , containers =
              [ kubernetes.Container::{
                , name = Constants.ProvenanceResolverName
                , image = Some resolver.image
            , imagePullPolicy = Some values.imagePullPolicy
                , securityContext = Some Constants.DropAllCapSecurityContext
                , env = Some resolverEnv
                , volumeMounts = Some resolverVolumeMounts
                }
              ]
            , volumes = Some resolverVolumes
            }
        )

in  [ kubernetes.Resource.ClusterRole buildOrchestratorClusterRole
    , kubernetes.Resource.ClusterRole kubeAgentClusterRole
    , kubernetes.Resource.Deployment buildDeployment
    , kubernetes.Resource.Deployment gitlabAgentDeployment
    , kubernetes.Resource.Deployment kubeAgentDeployment
    , kubernetes.Resource.Deployment linkerDeployment
    , kubernetes.Resource.Deployment resolverDeployment
    , kubernetes.Resource.RoleBinding buildOrchestratorRoleBinding
    , kubernetes.Resource.RoleBinding kubeAgentRoleBinding
    , kubernetes.Resource.Secret buildOrchestratorConfigSecret
    , kubernetes.Resource.Secret buildOrchestratorServiceAccountToken
    , kubernetes.Resource.Secret gitlabSecret
    , kubernetes.Resource.Secret graphSecret
    , kubernetes.Resource.Secret kubeAgentServiceAccountToken
    , kubernetes.Resource.Secret resolverSecret
    , kubernetes.Resource.Secret neo4jBoltCASecret
    , kubernetes.Resource.ServiceAccount buildOrchestratorServiceAccount
    , kubernetes.Resource.ServiceAccount kubeAgentServiceAccount
    ]
