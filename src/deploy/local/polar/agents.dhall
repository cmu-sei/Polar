let Agent = ../../types/agents.dhall

let kubernetes = ../../types/kubernetes.dhall

let Constants = ../../types/constants.dhall

let ProxyUtils = ../../types/proxy-utils.dhall

let proxyCACert = None Text

let deploymentName = Constants.ProvenanceDeploymentName

let values = ../values.dhall
let linker = values.linker
let resolver = values.resolver

let volumes =
      [ Constants.ClientTlsVolume ] # ProxyUtils.ProxyVolume proxyCACert

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

let linkerEnv = Constants.commonClientEnv # neo4jEnvVars

let resolverEnv = Constants.commonClientEnv # ProxyUtils.ProxyEnv proxyCACert

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
        ]
      # ProxyUtils.ProxyMount proxyCACert

let spec =
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
        , kubernetes.Container::{
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

in  kubernetes.Deployment::{
    , metadata = kubernetes.ObjectMeta::{
      , name = Some deploymentName
      , namespace = Some Constants.PolarNamespace
      , annotations = Some [ Constants.RejectSidecarAnnotation ]
      }
    , spec = Some kubernetes.DeploymentSpec::{
      , selector = kubernetes.LabelSelector::{
        , matchLabels = Some (toMap { name = deploymentName })
        }
      , replicas = Some 1
      , template = kubernetes.PodTemplateSpec::{
        , metadata = Some kubernetes.ObjectMeta::{
          , name = Some deploymentName
          , labels = Some [ { mapKey = "name", mapValue = deploymentName } ]
          }
        , spec = Some kubernetes.PodSpec::spec
        }
      }
    }
