let kubernetes = ../../types/kubernetes.dhall

let proxyUtils = ../../types/proxy-utils.dhall

let values = ../values.dhall

let Constants = ../../types/constants.dhall

let Agent = ../../types/agents.dhall

let gitlabAgentServiceAccount =
      kubernetes.ServiceAccount::{
      , apiVersion = "v1"
      , kind = "ServiceAccount"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.gitlab.serviceAccountName
        , namespace = Some values.namespace
        }
      }

let gitlabSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "gitlab-secret"
        , namespace = Some values.namespace
        }
      , immutable = Some True
      , stringData = Some
        [ { mapKey = values.gitlabSecret.key
          , mapValue = env:GITLAB_TOKEN as Text
          }
        ]
      , type = Some "Opaque"
      }

let proxyCACert = Some values.mtls.proxyCertificate

let volumes =
        [ kubernetes.Volume::{
          , name = values.gitlab.tls.certificateSpec.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some values.gitlab.tls.certificateSpec.secretName
            }
          }
        ]
      # proxyUtils.ProxyVolume proxyCACert

let observerEnv =
        Constants.commonClientEnv
      # [ kubernetes.EnvVar::{
          , name = "OBSERVER_BASE_INTERVAL"
          , value = Some "300"
          }
        , kubernetes.EnvVar::{
          , name = "GITLAB_ENDPOINT"
          , value = Some values.gitlab.observer.gitlabEndpoint
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
      # proxyUtils.ProxyEnv proxyCACert

let observerVolumeMounts =
        [ kubernetes.VolumeMount::{
          , name = values.gitlab.tls.certificateSpec.secretName
          , mountPath = values.tlsPath
          }
        ]
      # proxyUtils.ProxyMount proxyCACert

let consumerEnv =
        Constants.commonClientEnv
      # [ kubernetes.EnvVar::{
          , name = "GRAPH_ENDPOINT"
          , value = Some values.neo4jBoltAddr
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_DB"
          , value = Some values.gitlab.consumer.graph.graphDB
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_USER"
          , value = Some values.gitlab.consumer.graph.graphUsername
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_PASSWORD"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some values.gitlab.consumer.graph.graphPassword
            }
          }
        ]

let graphSecret = kubernetes.Secret::{
    apiVersion = "v1"
    , kind = "Secret"
    , metadata = kubernetes.ObjectMeta::{
        name = Some "polar-graph-pw"
        , namespace = Some values.namespace
        }
    -- , data : Optional (List { mapKey : Text, mapValue : Text })
    -- , immutable = Some True
    , stringData = Some [ { mapKey = values.graphSecret.key, mapValue = env:GRAPH_PASSWORD as Text } ]
    , type = Some "Opaque"
}

let gitlabAgentPod =
      kubernetes.PodSpec::{
      , imagePullSecrets = Some values.sandboxRegistry.imagePullSecrets
      , containers =
        [ kubernetes.Container::{
          , name = values.gitlab.observer.name
          , image = Some values.gitlab.observer.image
          , securityContext = Some values.gitlab.containerSecurityContext
          , env = Some observerEnv
          , volumeMounts = Some observerVolumeMounts
          }
        , kubernetes.Container::{
          , name = values.gitlab.consumer.name
          , image = Some values.gitlab.consumer.image
          , securityContext = Some values.gitlab.containerSecurityContext
          , env = Some consumerEnv
          , volumeMounts = Some
            [ kubernetes.VolumeMount::{
              , name = values.gitlab.tls.certificateSpec.secretName
              , mountPath = values.tlsPath
              , readOnly = Some True
              }
            ]
          }
        ]
      , volumes = Some volumes
      }

let gitlabAgentDeployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.gitlab.name
        , namespace = Some values.namespace
        , annotations = Some values.gitlab.podAnnotations
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = values.gitlab.name })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name = Some values.gitlab.name
            , labels = Some
              [ { mapKey = "name", mapValue = values.gitlab.name } ]
            }
          , spec = Some kubernetes.PodSpec::gitlabAgentPod
          }
        }
      }

let kubeAgentClusterRole =
      kubernetes.ClusterRole::{
      , apiVersion = "rbac.authorization.k8s.io/v1"
      , kind = "ClusterRole"
      , metadata = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }
      , rules = Some
        [ kubernetes.PolicyRule::{
          , apiGroups = Some [ "" ]
          , resources = Some [ "namespaces", "pods" ]
          , verbs = [ "get", "list", "watch" ]
          }
        ]
      }

let kubeAgentServiceAccount =
      kubernetes.ServiceAccount::{
      , apiVersion = "v1"
      , kind = "ServiceAccount"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.kubeAgent.observer.serviceAccountName
        , namespace = Some Constants.PolarNamespace
        }
      , automountServiceAccountToken = Some False
      }

let kubeAgentServiceAccountToken =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.kubeAgent.observer.secretName
        , namespace = Some Constants.PolarNamespace
        , annotations = Some
          [ { mapKey = "kubernetes.io/service-account.name"
            , mapValue = values.kubeAgent.observer.serviceAccountName
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

-- explicitly switch off mounting the procyCACert for the kube agent, we don't need one here.
let proxyCACert = None Text

let volumes =
        [ kubernetes.Volume::{
          , name = values.kubeAgent.tls.certificateSpec.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some values.kubeAgent.tls.certificateSpec.secretName
            }
          }
        , kubernetes.Volume::{
          , name = values.kubeAgent.observer.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some values.kubeAgent.observer.secretName
            }
          }
        ]
      # proxyUtils.ProxyVolume proxyCACert

let observerEnv =
        Constants.commonClientEnv
      # [ kubernetes.EnvVar::{
          , name = "KUBE_TOKEN"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some kubernetes.SecretKeySelector::{
              , name = Some values.kubeAgent.observer.secretName
              , key = "token"
              }
            }
          }
        ]
      # proxyUtils.ProxyEnv proxyCACert

let consumerEnv =
        Constants.commonClientEnv
      # [ kubernetes.EnvVar::{
          , name = "GRAPH_ENDPOINT"
          , value = Some values.neo4jBoltAddr
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_DB"
          , value = Some values.kubeAgent.consumer.graph.graphDB
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_USER"
          , value = Some values.kubeAgent.consumer.graph.graphUsername
          }
        , kubernetes.EnvVar::{
          , name = "GRAPH_PASSWORD"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some values.kubeAgent.consumer.graph.graphPassword
            }
          }
        ]

let observerVolumeMounts =
        [ kubernetes.VolumeMount::{
          , name = values.kubeAgent.tls.certificateSpec.secretName
          , mountPath = values.tlsPath
          }
        , kubernetes.VolumeMount::{
          , name = values.kubeAgent.observer.secretName
          , mountPath = "/var/run/secrets/kubernetes.io/serviceaccount"
          }
        ]
      # proxyUtils.ProxyMount proxyCACert

let spec =
      kubernetes.PodSpec::{
      , serviceAccountName = Some values.kubeAgent.observer.serviceAccountName
      , imagePullSecrets = Some values.sandboxRegistry.imagePullSecrets
      , containers =
        [ kubernetes.Container::{
          , name = values.kubeAgent.observer.name
          , image = Some values.kubeAgent.observer.image
          , securityContext = Some values.kubeAgent.containerSecurityContext
          , env = Some observerEnv
          , volumeMounts = Some observerVolumeMounts
          }
        , kubernetes.Container::{
          , name = values.kubeAgent.consumer.name
          , image = Some values.kubeAgent.consumer.image
          , securityContext = Some values.kubeAgent.containerSecurityContext
          , env = Some consumerEnv
          , volumeMounts = Some
            [ kubernetes.VolumeMount::{
              , name = values.kubeAgent.tls.certificateSpec.secretName
              , mountPath = values.tlsPath
              , readOnly = Some True
              }
            ]
          }
        ]
      , volumes = Some volumes
      }

let kubeAgentDeployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.kubeAgent.name
        , namespace = Some Constants.PolarNamespace
        , annotations = Some values.kubeAgent.podAnnotations
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = values.kubeAgent.name })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name = Some values.kubeAgent.name
            , labels = Some
              [ { mapKey = "name", mapValue = values.kubeAgent.name } ]
            }
          , spec = Some kubernetes.PodSpec::spec
          }
        }
      }

---- PROVENANCE ---
-- A secret value contianing the docker config.json we want to use to give the resolver access to registries
      let resolverOciSecret =
            kubernetes.Secret::{
            , apiVersion = "v1"
            , kind = "Secret"
            , metadata = kubernetes.ObjectMeta::{
              , name = Some Constants.OciRegistrySecret.name
              , namespace = Some Constants.PolarNamespace
              }
            , stringData = Some
              [ { mapKey = "config.json"
                , mapValue = env:DOCKER_AUTH_JSON as Text
                }
              ]
            , immutable = Some True
            , type = Some "Opaque"
            }

let proxyCACert = Some values.mtls.proxyCertificate


let linker
    : Agent.ProvenanceLinker
    = { name = Constants.ProvenanceLinkerName
      , image =
          "${Constants.SandboxRegistry.url}/polar/polar-linker-agent:${Constants.commitSha}"
      , graph = Constants.graphConfig
      , tls = Constants.commonClientTls
      }

let resolver
    : Agent.ProvenanceResolver
    = { name = Constants.ProvenanceLinkerName
      , image =
          "${Constants.SandboxRegistry.url}/polar/polar-resolver-agent:${Constants.commitSha}"
      , tls = Constants.commonClientTls
      }

let volumes = [
    kubernetes.Volume::{
    , name = Constants.OciRegistrySecret.name
    , secret = Some kubernetes.SecretVolumeSource::{
    , secretName = Some Constants.OciRegistrySecret.name
    }
}
, Constants.ClientTlsVolume ] # proxyUtils.ProxyVolume proxyCACert

let linkerEnv = Constants.commonClientEnv # Constants.graphClientEnvVars # [ values.graphEndpointEnvVar ]

let resolverEnv = Constants.commonClientEnv # proxyUtils.ProxyEnv proxyCACert


let linkerVolumeMounts =
      [ kubernetes.VolumeMount::{
        , name = Constants.CassiniServerCertificateSecret
        , mountPath = Constants.tlsPath
        }
      ]

let resolverVolumeMounts =
        [ kubernetes.VolumeMount::{
          , name = Constants.CassiniServerCertificateSecret
          , mountPath = Constants.tlsPath
          }
          , kubernetes.VolumeMount::{
          , name = Constants.OciRegistrySecret.name
          , mountPath = "/home/polar/.docker/"
          }
        ]
      # proxyUtils.ProxyMount proxyCACert


let spec =
      kubernetes.PodSpec::{
      , imagePullSecrets = Some Constants.SandboxRegistry.imagePullSecrets
      , containers =
        [ kubernetes.Container::{
          , name = Constants.ProvenanceLinkerName
          , image = Some linker.image
          , securityContext = Some Constants.DropAllCapSecurityContext
          , env = Some linkerEnv
          , volumeMounts = Some linkerVolumeMounts
          }
        , kubernetes.Container::{
          , name = Constants.ProvenanceResolverName
          , image = Some resolver.image
          , securityContext = Some Constants.DropAllCapSecurityContext
          , env = Some resolverEnv
          , volumeMounts = Some resolverVolumeMounts
          }
        ]
      , volumes = Some volumes
      }

    let resolverSpec =
        kubernetes.PodSpec::{
        , containers =
            [
            kubernetes.Container::{
            , name = Constants.ProvenanceResolverName
            , image = Some resolver.image
            , imagePullPolicy = Some "Never"
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env = Some resolverEnv
            , volumeMounts = Some resolverVolumeMounts
            }
            ]
        , volumes = Some volumes
        }

    let linkerSpec =
        kubernetes.PodSpec::{
        , containers =
            [ kubernetes.Container::{
            , name = Constants.ProvenanceLinkerName
            , image = Some linker.image
            , imagePullPolicy = Some "Never"
            , securityContext = Some Constants.DropAllCapSecurityContext
            , env = Some linkerEnv
            , volumeMounts = Some linkerVolumeMounts
            }
            ]
        , volumes = Some volumes
        }

    let resolverDeployment =  kubernetes.Deployment::{
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
            , labels = Some [ { mapKey = "name", mapValue = Constants.RegistryResolverName } ]
            }
            , spec = Some kubernetes.PodSpec::resolverSpec
            }
        }
        }

    let linkerDeployment =  kubernetes.Deployment::{
        , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.ArtifactLinkerName
        , namespace = Some Constants.PolarNamespace
        , annotations = Some [ Constants.RejectSidecarAnnotation ]
        }
        , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
            , matchLabels = Some (toMap { name = Constants.ArtifactLinkerName})
            }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
            , metadata = Some kubernetes.ObjectMeta::{
            , name = Some Constants.ArtifactLinkerName
            , labels = Some [ { mapKey = "name", mapValue = Constants.ArtifactLinkerName } ]
            }
            , spec = Some kubernetes.PodSpec::linkerSpec
            }
        }
        }


in  [ kubernetes.Resource.Deployment gitlabAgentDeployment
    , kubernetes.Resource.Secret graphSecret
    , kubernetes.Resource.Secret gitlabSecret
    , kubernetes.Resource.ServiceAccount gitlabAgentServiceAccount
    , kubernetes.Resource.ClusterRole kubeAgentClusterRole
    , kubernetes.Resource.ServiceAccount kubeAgentServiceAccount
    , kubernetes.Resource.RoleBinding kubeAgentRoleBinding
    , kubernetes.Resource.Secret kubeAgentServiceAccountToken
    , kubernetes.Resource.Deployment kubeAgentDeployment
    , kubernetes.Resource.Secret resolverOciSecret
    , kubernetes.Resource.Deployment linkerDeployment
    , kubernetes.Resource.Deployment resolverDeployment
    ]
