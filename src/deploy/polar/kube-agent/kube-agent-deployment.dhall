let kubernetes = ../../types/kubernetes.dhall
let proxyUtils = ../../types/proxy-utils.dhall
let values = ../values.dhall
-- define an optional type for the CA cert it may or may not be provided in the future
-- let proxyCACert = Some values.mtls.proxyCertificate
let proxyCACert = None Text 
-- static env vars - required by both services for operation
-- When we add agents, this should be broken out to be shared
let CommonEnv = [
  kubernetes.EnvVar::{ name = "TLS_CA_CERT", value = Some values.mtls.caCertPath }
, kubernetes.EnvVar::{ name = "TLS_CLIENT_CERT", value = Some values.mtls.serverCertPath }
, kubernetes.EnvVar::{ name = "TLS_CLIENT_KEY", value = Some values.mtls.serverKeyPath }
, kubernetes.EnvVar::{ name = "BROKER_ADDR", value = Some values.cassiniAddr }
, kubernetes.EnvVar::{ name = "CASSINI_SERVER_NAME", value = Some values.cassiniDNSName }
]

-- Volumes accessible to our kubeAgent agent
let volumes =
      [ 
        -- our root-ca-secret for communicating with cassini 
        kubernetes.Volume::{
          , name = values.kubeAgent.tls.certificateSpec.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
              secretName = Some values.kubeAgent.tls.certificateSpec.secretName
            }
        }
        , kubernetes.Volume::{
          , name = values.kubeAgent.observer.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
              secretName = Some values.kubeAgent.observer.secretName
            }
        }
      ]
    # proxyUtils.ProxyVolume proxyCACert

let observerEnv =
      CommonEnv
    # [ 
      kubernetes.EnvVar::{
        name = "KUBE_TOKEN"
        , valueFrom = Some kubernetes.EnvVarSource::{
          secretKeyRef = Some kubernetes.SecretKeySelector::{
            name = Some values.kubeAgent.observer.secretName
          , key = "token"
          }
        }
      } 
      ]
    # proxyUtils.ProxyEnv proxyCACert

let consumerEnv 
  = CommonEnv # 
    [
      kubernetes.EnvVar::{
          name = "GRAPH_ENDPOINT"
          , value = Some values.neo4jBoltAddr
      }
      , kubernetes.EnvVar::{
          name = "GRAPH_DB"
          , value = Some values.kubeAgent.consumer.graph.graphDB
      }
      , kubernetes.EnvVar::{
          name = "GRAPH_USER"
          , value = Some values.kubeAgent.consumer.graph.graphUsername                       
      }
      , kubernetes.EnvVar::{
          name = "GRAPH_PASSWORD"
          , valueFrom = Some kubernetes.EnvVarSource::{
              secretKeyRef = Some values.kubeAgent.consumer.graph.graphPassword
          }
      }             
    ]

let observerVolumeMounts =
      [ 
        , kubernetes.VolumeMount::{ name = values.kubeAgent.tls.certificateSpec.secretName, mountPath = values.tlsPath }
        -- mount the agent's serviceaccount token
        , kubernetes.VolumeMount::{ name = values.kubeAgent.observer.secretName, mountPath = "/var/run/secrets/kubernetes.io/serviceaccount" }
      ]
    # proxyUtils.ProxyMount proxyCACert


let spec 
  = kubernetes.PodSpec::{
  , serviceAccountName = Some values.kubeAgent.observer.serviceAccountName
  , imagePullSecrets = Some values.sandboxRegistry.imagePullSecrets
  , containers =
    [ 
      , kubernetes.Container::{
          , name = values.kubeAgent.observer.name
          , image = Some  values.kubeAgent.observer.image
          , securityContext = Some values.kubeAgent.containerSecurityContext
          , env = Some observerEnv
          , volumeMounts = Some observerVolumeMounts
      }
      , kubernetes.Container::{
          name = values.kubeAgent.consumer.name
          , image = Some values.kubeAgent.consumer.image
          , securityContext = Some values.kubeAgent.containerSecurityContext
          , env = Some consumerEnv
          , volumeMounts = Some [
              -- mount cassini secret
              kubernetes.VolumeMount::{
                  name  = values.kubeAgent.tls.certificateSpec.secretName
                  , mountPath = values.tlsPath
                  , readOnly = Some True
              }
          ]
      }
    ]
  , volumes = Some volumes
}

let 
    deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        name = Some values.kubeAgent.name
        , namespace = Some values.namespace
        , annotations = Some values.kubeAgent.podAnnotations
       }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = values.kubeAgent.name })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
                name = Some values.kubeAgent.name
                , labels = Some [ { mapKey = "name", mapValue = values.kubeAgent.name } ]
            }
          , spec = Some kubernetes.PodSpec::spec
          }
        }
      }
    
in  deployment