let Agent = ../../types/agents.dhall

let kubernetes = ../../types/kubernetes.dhall

let Constants = ../../types/Constants.dhall

let ProxyUtils = ../../types/proxy-utils.dhall

let proxyCACert = Some Constants.mtls.proxyCertificate

let deploymentName = Constants.ProvenanceDeploymentName

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

let volumes = [ Constants.ClientTlsVolume ] # ProxyUtils.ProxyVolume proxyCACert

let linkerEnv = Constants.commonClientEnv

let resolverEnv = Constants.commonClientEnv # ProxyUtils.ProxyEnv proxyCACert

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
