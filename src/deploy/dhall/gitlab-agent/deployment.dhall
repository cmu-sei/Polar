let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

-- Define a volume to load a proxy CA cert if one is provided
let ProxyVolume =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(certName : Text) →
                [ kubernetes.Volume::{
                    , name = "proxy-ca-cert"
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
              λ(_ : Text) →
                [ kubernetes.VolumeMount::{
                    , name = "proxy-ca-cert"
                    , mountPath = "/etc/ssl/"
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
                    , value = Some "/etc/ssl/proxy_ca_certificate.pem"
                  }
                ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          cert

-- Volumes accessible to our gitlab agent
let volumes =
      [ kubernetes.Volume::{
          , name = "client-mtls"
          , secret = Some kubernetes.SecretVolumeSource::{
              secretName = Some "client-mtls"
            }
        }
      ]
    # ProxyVolume values.gitlab.proxyCertificate

let observerEnv =
      [ -- static env vars - required for operation...
        , kubernetes.EnvVar::{ name = "TLS_CA_CERT", value = Some "/etc/tls/ca_certificate.pem" }
        , kubernetes.EnvVar::{ name = "TLS_CLIENT_CERT", value = Some "/etc/tls/client_polar_certificate.pem" }
        , kubernetes.EnvVar::{ name = "TLS_CLIENT_KEY", value = Some "/etc/tls/client_polar_key.pem" }
        , kubernetes.EnvVar::{ name = "BROKER_ADDR", value = Some values.cassiniAddr }
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
    # ProxyEnv values.gitlab.proxyCertificate

let observerVolumeMounts =
      [ kubernetes.VolumeMount::{ name = "client-mtls", mountPath = "/etc/tls/" } ]
    # ProxyMount values.gitlab.proxyCertificate



let gitlabAgentPod = kubernetes.PodSpec::{
            , containers =
              [ 
                , kubernetes.Container::{
                    , name = values.gitlab.observer.name
                    , image = Some  values.gitlab.observer.image
                    , env = Some observerEnv
                    , volumeMounts = Some observerVolumeMounts
                }
                , kubernetes.Container::{
                    name = values.gitlab.consumer.name
                    , image = Some values.gitlab.consumer.image
                    , env = Some [
                        kubernetes.EnvVar::{
                            name = "TLS_CA_CERT"
                            , value = Some "/etc/tls/ca_certificate.pem"
                        }
                        , kubernetes.EnvVar::{
                            name = "TLS_CLIENT_CERT"
                            , value = Some "/etc/tls/client_polar_certificate.pem"
                        }
                        , kubernetes.EnvVar::{
                            name = "TLS_CLIENT_KEY"
                            , value = Some "/etc/tls/client_polar_key.pem"
                        }
                        , kubernetes.EnvVar::{
                            name = "BROKER_ADDR"
                            , value = Some values.cassiniAddr
                        }
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
                    ]
                    , volumeMounts = Some [
                        kubernetes.VolumeMount::{
                            name  = "client-mtls"
                            , mountPath = "/etc/tls/"
                            , readOnly = Some True
                        }
                    ]
                }
              ]
            , volumes = Some [
                kubernetes.Volume::{
                    , name = "client-mtls"
                    , secret = Some kubernetes.SecretVolumeSource::{
                        secretName = Some "client-mtls"
                    }
                }
                , kubernetes.Volume::{
                    , name = "proxy-ca-cert"
                    , secret = Some kubernetes.SecretVolumeSource::{
                        secretName = values.gitlab.proxyCertificate
                    }
                }
            ]
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
            ,   labels = Some [ { mapKey = "name", mapValue = values.gitlab.name } ]
            }
          , spec = Some kubernetes.PodSpec::gitlabAgentPod
          }
        }
      }
    
in  deployment