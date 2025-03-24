let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let 
    deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        name = Some "cassini"
        , namespace = Some "polar"
       }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = "cassini" })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
                name = Some "cassini"
            ,   labels = Some [ { mapKey = "name", mapValue = "cassini" } ]
            }
          , spec = Some kubernetes.PodSpec::{
            , containers =
              [ 
                kubernetes.Container::{
                , name = "cassini"
                , image = Some "localhost/cassini:0.1.0"
                , env = Some [
                    kubernetes.EnvVar::{
                        name = "TLS_CA_CERT"
                        , value = Some "/etc/cassini/tls/ca_certificate.pem"
                    }
                    , kubernetes.EnvVar::{
                        name = "TLS_SERVER_CERT_CHAIN"
                        , value = Some "/etc/cassini/tls/server_polar_certificate.pem"
                    }
                    , kubernetes.EnvVar::{
                        name = "TLS_SERVER_KEY"
                        , value = Some "/etc/cassini/tls/server_polar_key.pem"
                    }
                    , kubernetes.EnvVar::{
                        name = "CASSINI_BIND_ADDR"
                        , value = Some "0.0.0.0:8080"
                    }
                ]
                , ports = Some
                  [ kubernetes.ContainerPort::{ containerPort = 8080 } ]
                , volumeMounts = Some [
                    kubernetes.VolumeMount::{
                        name  = "mtls-secrets"
                        , mountPath = "/etc/cassini/tls"
                        , readOnly = Some True
                        
                    }
                ]
                },
              ]
            , volumes = Some [
                , kubernetes.Volume::{
                    , name = "mtls-secrets"
                    , secret = Some kubernetes.SecretVolumeSource::{
                        secretName = Some "cassini-mtls"
                    }
                }
            ]
            }
          }
        }
      }
    
in  deployment