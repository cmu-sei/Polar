let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let 
    deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        name = Some values.cassini.name
        , namespace = Some values.namespace
       }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = values.cassini.name })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
                name = Some values.cassini.name
            ,   labels = Some [ { mapKey = "name", mapValue = values.cassini.name } ]
            }
          , spec = Some kubernetes.PodSpec::{
            , containers =
              [ 
                kubernetes.Container::{
                , name = "cassini"
                , image = Some values.cassini.image
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
                        , value = Some "0.0.0.0:${Natural/show values.cassini.port}"
                    }
                ]
                , ports = Some
                  [ kubernetes.ContainerPort::{ containerPort = values.cassini.port } ]
                , volumeMounts = Some [
                    kubernetes.VolumeMount::{
                        name  = "mtls-secrets"
                        , mountPath = "/etc/cassini/tls"
                        , readOnly = Some True
                        
                    }
                ]
                },
              ]
            , volumes = Some values.cassini.volumes
            }
          }
        }
      }
    
in  deployment