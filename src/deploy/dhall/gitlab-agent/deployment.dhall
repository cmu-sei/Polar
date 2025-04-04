let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let gitlabAgentPod = kubernetes.PodSpec::{
            , containers =
              [ 
                , kubernetes.Container::{
                    , name = values.gitlab.observer.name
                    , image = Some  values.gitlab.observer.image
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
                            name = "GITLAB_ENDPOINT"
                            , value = Some values.gitlab.observer.gitlabEndpoint
                        }
                        , kubernetes.EnvVar::{
                            name = "GITLAB_TOKEN"
                            , valueFrom = Some kubernetes.EnvVarSource::{
                                secretKeyRef = Some kubernetes.SecretKeySelector::{
                                    key = "token"
                                    , name = Some "gitlab-secret"
                                }
                            }
                        }           
                    ]
                    , volumeMounts = Some [
                        kubernetes.VolumeMount::{
                            name  = "client-mtls"
                            , mountPath = "/etc/tls/"
                            
                        }
                    ]
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
                            , value = Some values.neo4jAddr
                        }
                        , kubernetes.EnvVar::{
                            name = "GRAPH_DB"
                            , value = Some values.gitlab.consumer.graph.graphDB
                        }
                        , kubernetes.EnvVar::{
                            name = "GRAPH_USERNAME"
                            , value = Some values.gitlab.consumer.graph.graphUsername                       
                        }
                        , kubernetes.EnvVar::{
                            name = "GRAPH_PASSWORD"
                            , valueFrom = Some kubernetes.EnvVarSource::{
                                secretKeyRef = Some kubernetes.SecretKeySelector::values.gitlab.consumer.graph.graphSecret
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
            ]
}

let 
    deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        name = Some values.gitlab.name
        , namespace = Some values.namespace
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