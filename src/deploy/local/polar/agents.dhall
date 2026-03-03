let Agent = ../../types/agents.dhall

let kubernetes = ../../types/kubernetes.dhall

let Constants = ../../types/constants.dhall

let functions = ../../types/functions.dhall

let values = ../values.dhall

let linker = values.linker

let resolver = values.resolver

-- Name of the secret containing a proxy ca certificate to be mounted into containers that need it.
let proxyCACert = Some "proxy-ca-cert"

let volumes = Constants.commonVolumes # functions.ProxyVolume proxyCACert

let volumeMounts =
      [ values.cassiniTlsVolumeMount ] # functions.ProxyMount proxyCACert

let envVars = [
kubernetes.EnvVar::{
  , name = "RUST_LOG"
  , value = Some "debug"
  }
] # Constants.commonClientEnv

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
      ]

let gitlabSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "gitlab-secret"
        , namespace = Some Constants.PolarNamespace
        }
      , immutable = Some True
      , stringData = Some
        [ { mapKey = Constants.gitlabSecretKeySelector.key
          , mapValue = env:GITLAB_TOKEN as Text
          }
        ]
      , type = Some "Opaque"
      }

let observerEnv =
        envVars
      # functions.ProxyEnv proxyCACert
      # [ kubernetes.EnvVar::{
          , name = "OBSERVER_BASE_INTERVAL"
          , value = Some "30"
          }
        , kubernetes.EnvVar::{
          , name = "GITLAB_ENDPOINT"
          , value = Some values.gitlabObserver.endpoint
          }
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

let consumerEnv =
        envVars
      # [ kubernetes.EnvVar::{
          , name = "GRAPH_ENDPOINT"
          , value = Some values.neo4jBoltAddr
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_DB"
          , value = Some values.gitlabConsumer.graph.graphDB
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_USER"
          , value = Some values.gitlabConsumer.graph.graphUsername
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_PASSWORD"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some Constants.graphSecretKeySelector
            }
          }
        ]

let graphSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "polar-graph-pw"
        , namespace = Some Constants.PolarNamespace
        }
      , stringData = Some
        [ { mapKey = Constants.neo4jSecret.key
          , mapValue = env:GRAPH_PASSWORD as Text
          }
        ]
      , type = Some "Opaque"
      }

let gitlabAgentVolumes = Constants.commonVolumes # functions.ProxyVolume proxyCACert
let gitlabAgentPod =
      kubernetes.PodSpec::{
      , imagePullSecrets = Some values.imagePullSecrets
      , containers =
        [ kubernetes.Container::{
          , name = values.gitlabObserver.name
          , image = Some values.gitlabObserver.image
          , securityContext = Some Constants.DropAllCapSecurityContext
          , env = Some observerEnv
          , volumeMounts = Some volumeMounts
          }
        , kubernetes.Container::{
          , name = values.gitlabConsumer.name
          , image = Some values.gitlabConsumer.image
          , securityContext = Some Constants.DropAllCapSecurityContext
          , env = Some consumerEnv
          , volumeMounts = Some volumeMounts
          }
        ]
      , volumes = Some gitlabAgentVolumes
      }

let gitlabAgentDeployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "gitlab-agents"
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = "gitlab-agents" })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name = Some "gitlab-agents"
            , labels = Some [ { mapKey = "name", mapValue = "gitlab-agents" } ]
            }
          , spec = Some kubernetes.PodSpec::gitlabAgentPod
          }
        }
      }

let gitObserverEnv =
        envVars
      # [ kubernetes.EnvVar::{
          , name = "GIT_AGENT_CONFIG"
          , value = Some "git.json"
          }
        ]

let gitObserverSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.gitObserverSecretName
        , namespace = Some Constants.PolarNamespace
        }
      , stringData = Some
        [ { mapKey = "git.json", mapValue = values.gitObserver.config } ]
      , immutable = Some True
      , type = Some "Opaque"
      }

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

let subject =
      kubernetes.Subject::{
      , kind = "ServiceAccount"
      , name = "kube-observer-sa"
      , namespace = Some Constants.PolarNamespace
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
      , subjects = Some [ subject ]
      }

let kubeAgentVolumes =
        volumes
      # [ kubernetes.Volume::{
          , name = values.kubeObserver.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some values.kubeObserver.secretName
            }
          }
        ]

let observerEnv =
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

let consumerEnv =
        Constants.commonClientEnv
      # [ kubernetes.EnvVar::{
          , name = "GRAPH_ENDPOINT"
          , value = Some values.neo4jBoltAddr
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_DB"
          , value = Some values.kubeConsumer.graph.graphDB
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_USER"
          , value = Some values.kubeConsumer.graph.graphUsername
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_PASSWORD"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some Constants.graphSecretKeySelector
            }
          }
        ]

let kubeObserverVolumeMounts =
        [ kubernetes.VolumeMount::{
          , name = values.kubeObserver.secretName
          , mountPath = "/var/run/secrets/kubernetes.io/serviceaccount"
          }
        ]
      # volumeMounts

let spec =
      kubernetes.PodSpec::{
      , imagePullSecrets = Some values.imagePullSecrets
      , serviceAccountName = Some values.kubeObserver.serviceAccountName
      , containers =
        [ kubernetes.Container::{
          , name = values.kubeObserver.name
          , image = Some values.kubeObserver.image
          , securityContext = Some Constants.DropAllCapSecurityContext
          , env = Some observerEnv
          , volumeMounts = Some kubeObserverVolumeMounts
          }
        , kubernetes.Container::{
          , name = values.kubeConsumer.name
          , image = Some values.kubeConsumer.image
          , securityContext = Some Constants.DropAllCapSecurityContext
          , env = Some consumerEnv
          , volumeMounts = Some volumeMounts
          }
        ]
      , volumes = Some kubeAgentVolumes
      }

let kubeAgentDeployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "kube-agent"
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = "kube-agent" })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name = Some "kube-agent"
            , labels = Some [ { mapKey = "name", mapValue = "kube-agent" } ]
            }
          , spec = Some kubernetes.PodSpec::spec
          }
        }
      }

let linkerEnv = envVars # neo4jEnvVars

let resolverEnv = envVars # functions.ProxyEnv proxyCACert

let resolverSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.OciRegistrySecret.name
        , namespace = Some Constants.PolarNamespace
        }
      , stringData = Some
        [ { mapKey = Constants.OciRegistrySecret.name
          , mapValue = Constants.OciRegistrySecret.value
          }
        ]
      , immutable = Some True
      , type = Some "Opaque"
      }

let resolverVolumes =
        volumes
      # [ kubernetes.Volume::{
          , name = Constants.OciRegistrySecret.name
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some Constants.OciRegistrySecret.name
            , items = Some [ kubernetes.KeyToPath::{ key = "oci-registry-auth", path = "config.json" } ]
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

let resolverSpec =
      kubernetes.PodSpec::{
      , imagePullSecrets = Some values.imagePullSecrets
      , containers =
        [ kubernetes.Container::{
          , name = Constants.ProvenanceResolverName
          , image = Some resolver.image
          , securityContext = Some Constants.DropAllCapSecurityContext
          , env = Some resolverEnv
          , volumeMounts = Some resolverVolumeMounts
          }
        ]
      , volumes = Some resolverVolumes
      }

let linkerSpec =
      kubernetes.PodSpec::{
      , imagePullSecrets = Some values.imagePullSecrets
      , containers =
        [ kubernetes.Container::{
          , name = Constants.ProvenanceLinkerName
          , image = Some linker.image
          , securityContext = Some Constants.DropAllCapSecurityContext
          , env = Some linkerEnv
          , volumeMounts = Some volumeMounts
          }
        ]
      , volumes = Some volumes
      }

let resolverDeployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.RegistryResolverName
        , namespace = Some Constants.PolarNamespace
        , annotations = Some [ Constants.RejectSidecarAnnotation ]
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = Constants.RegistryResolverName })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name = Some Constants.RegistryResolverName
            , labels = Some
              [ { mapKey = "name", mapValue = Constants.RegistryResolverName } ]
            }
          , spec = Some kubernetes.PodSpec::resolverSpec
          }
        }
      }

let linkerDeployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.ArtifactLinkerName
        , namespace = Some Constants.PolarNamespace
        , annotations = Some [ Constants.RejectSidecarAnnotation ]
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = Constants.ArtifactLinkerName })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name = Some Constants.ArtifactLinkerName
            , labels = Some
              [ { mapKey = "name", mapValue = Constants.ArtifactLinkerName } ]
            }
          , spec = Some kubernetes.PodSpec::linkerSpec
          }
        }
      }

in  [ kubernetes.Resource.Deployment gitlabAgentDeployment
    , kubernetes.Resource.Secret graphSecret
    , kubernetes.Resource.Secret gitlabSecret
    , kubernetes.Resource.ClusterRole kubeAgentClusterRole
    , kubernetes.Resource.ServiceAccount kubeAgentServiceAccount
    , kubernetes.Resource.RoleBinding kubeAgentRoleBinding
    , kubernetes.Resource.Secret kubeAgentServiceAccountToken
    , kubernetes.Resource.Deployment kubeAgentDeployment
    , kubernetes.Resource.Secret resolverSecret
    , kubernetes.Resource.Deployment linkerDeployment
    --, kubernetes.Resource.Deployment resolverDeployment
    ]
