let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

-- define an optional type for the CA cert it may or may not be provided in the future
let proxyCACert = Some values.mtls.proxyCertificate

-- static env vars - required by both services for operation
-- When we add agents, this should be broken out to be shared
let CommonEnv = [
  kubernetes.EnvVar::{ name = "TLS_CA_CERT", value = Some values.mtls.caCertPath }
, kubernetes.EnvVar::{ name = "TLS_CLIENT_CERT", value = Some values.mtls.serverCertPath }
, kubernetes.EnvVar::{ name = "TLS_CLIENT_KEY", value = Some values.mtls.serverKeyPath }
, kubernetes.EnvVar::{ name = "BROKER_ADDR", value = Some values.cassiniAddr }
, kubernetes.EnvVar::{ name = "CASSINI_SERVER_NAME", value = Some values.cassiniDNSName }
]

-- Define a volume to load a proxy CA cert if one is provided
let ProxyVolume =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(certName : Text) →
                [ kubernetes.Volume::{
                    , name = certName
                    , secret = Some kubernetes.SecretVolumeSource::{
                        secretName = Some certName
                    }
                  }
                ]
          , None = [] : List kubernetes.Volume.Type
          }
          cert

let ProxyMount =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(cert : Text) →
                [ kubernetes.VolumeMount::{
                    , name = cert
                    , mountPath = "/etc/tls/proxy"
                    , readOnly = Some True
                  }
                ]
          , None = [] : List kubernetes.VolumeMount.Type
          }
          cert

let ProxyEnv =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(_ : Text) →
                [ kubernetes.EnvVar::{
                    , name = "PROXY_CA_CERT"
                    , value = Some "${values.tlsPath}/proxy/proxy.crt"
                  }
                ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          cert

-- Volumes accessible to our gitlab agent
let volumes =
      [ 
        -- our root-ca-secret for communicating with cassini 
        kubernetes.Volume::{
          , name = values.gitlab.tls.certificateSpec.secretName
          , secret = Some kubernetes.SecretVolumeSource::{
              secretName = Some values.gitlab.tls.certificateSpec.secretName
            }
        }
      ]
    # ProxyVolume proxyCACert

let observerEnv =
      CommonEnv
      # [
        , kubernetes.EnvVar::{ name = "GITLAB_ENDPOINT", value = Some values.gitlab.observer.gitlabEndpoint }
        , kubernetes.EnvVar::{
            name = "GITLAB_TOKEN"
          , valueFrom = Some kubernetes.EnvVarSource::{
              secretKeyRef = Some kubernetes.SecretKeySelector::{
                name = Some "gitlab-secret"
              , key = "token"
              }
            }
          }
      ]
    # ProxyEnv proxyCACert

let consumerEnv = CommonEnv # 
              [
              , kubernetes.EnvVar::{
                  name = "GRAPH_ENDPOINT"
                  , value = Some values.neo4jBoltAddr
              }
              , kubernetes.EnvVar::{
                  name = "GRAPH_DB"
                  , value = Some values.gitlab.consumer.graph.graphDB
              }
              , kubernetes.EnvVar::{
                  name = "GRAPH_USER"
                  , value = Some values.gitlab.consumer.graph.graphUsername                       
              }
              , kubernetes.EnvVar::{
                  name = "GRAPH_PASSWORD"
                  , valueFrom = Some kubernetes.EnvVarSource::{
                      secretKeyRef = Some values.gitlab.consumer.graph.graphPassword
                  }
              }
              -- TODO: Write some logic to provide this optional value.
              -- If the graph is behind a proxy or service mesh, we have to set this
              -- , kubernetes.EnvVar::{
              --     name = "GRAPH_CA_CERT"
              --     , value = Some "/graph/tls/proxy.crt"
              -- }
                            
          ]
let observerVolumeMounts =
      [ kubernetes.VolumeMount::{ name = values.gitlab.tls.certificateSpec.secretName, mountPath = values.tlsPath } ]
    # ProxyMount proxyCACert


let gitlabAgentPod 
  = kubernetes.PodSpec::{
  , imagePullSecrets = Some values.sandboxRegistry.imagePullSecrets
  , containers =
    [ 
      , kubernetes.Container::{
          , name = values.gitlab.observer.name
          , image = Some  values.gitlab.observer.image
          , securityContext = Some values.gitlab.containerSecurityContext
          , env = Some observerEnv
          , volumeMounts = Some observerVolumeMounts
      }
      , kubernetes.Container::{
          name = values.gitlab.consumer.name
          , image = Some values.gitlab.consumer.image
          , securityContext = Some values.gitlab.containerSecurityContext
          , env = Some consumerEnv
          , volumeMounts = Some [
              -- mount cassini secret
              kubernetes.VolumeMount::{
                  name  = values.gitlab.tls.certificateSpec.secretName
                  , mountPath = values.tlsPath
                  , readOnly = Some True
              }
              -- , kubernetes.VolumeMount::{
              --   name = values.mtls.proxyCertificate
              --   , mountPath = "/graph/tls/"
              -- }
          ]
      }
    ]
  , volumes = Some volumes
}

let 
    deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        name = Some values.gitlab.name
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
                name = Some values.gitlab.name
                , labels = Some [ { mapKey = "name", mapValue = values.gitlab.name } ]
            }
          , spec = Some kubernetes.PodSpec::gitlabAgentPod
          }
        }
      }
    
in  deployment