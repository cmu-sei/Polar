let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let gitlabAgentPod = {
            , containers =
              [ 
                kubernetes.Container::{
                    , name = "gitlab-observer"
                    , image = Some "localhost/polar-gitlab-observer:0.1.0"
                    , env = Some [
                        kubernetes.EnvVar::{
                            name = "TLS_CA_CERT"
                            , value = Some "/etc/cassini/tls/ca_certificate.pem"
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
                            , value = Some "cassini-ip-svc.polar.svc.cluster.local:8080"
                        }
                        , kubernetes.EnvVar::{
                            name = "GITLAB_ENDPOINT"
                            , value = Some "https://gitlab.sandbox.labz.s-box.org/api/graphql"
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
                            , mountPath = "/etc/tls"
                            
                        }
                    ]
                }
                , kubernetes.Container::{
                    name = "gitlab-consumer"
                    , image = Some "localhost/polar-gitlab-consumer:0.1.0"
                    , env = Some [
                        kubernetes.EnvVar::{
                            name = "TLS_CA_CERT"
                            , value = Some "/etc/cassini/tls/ca_certificate.pem"
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
                            , value = Some "cassini-ip-svc.polar.svc.cluster.local:8080"
                        }
                        , kubernetes.EnvVar::{
                            name = "GRAPH_ENDPOINT"
                            , value = Some "neo4j-svc.polar.svc.cluster.local:7474"
                        }
                        , kubernetes.EnvVar::{
                            name = "GRAPH_DB"
                            , value = Some "neo4j"
                        }
                        , kubernetes.EnvVar::{
                            name = "GRAPH_USERNAME"
                            , value = Some "neo4j"                        
                        }
                        , kubernetes.EnvVar::{
                            name = "GRAPH_PASSWORD"
                            , valueFrom = Some kubernetes.EnvVarSource::{
                                secretKeyRef = Some kubernetes.SecretKeySelector::{
                                    key = "token"
                                    , name = Some "neo4j-secret"
                                }
                            }
                        }
                    ]
                    , volumeMounts = Some [
                        kubernetes.VolumeMount::{
                            name  = "client-mtls"
                            , mountPath = "/etc/tls"
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

let podTemplate = {
          , metadata = Some kubernetes.ObjectMeta::{
                name = Some "gitlab-agent"
            ,   labels = Some [ { mapKey = "name", mapValue = "gitlab-agent" } ]
            }
          , spec = Some kubernetes.PodSpec::gitlabAgentPod
          }

let deploymentSpec = {
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = "gitlab-agent" })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::podTemplate
        }

let 
    deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        name = Some "gitlab-agent"
        , namespace = Some "polar"
       }
      , spec = Some kubernetes.DeploymentSpec::deploymentSpec
      }
    
in  deployment