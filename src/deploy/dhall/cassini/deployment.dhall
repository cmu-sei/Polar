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
                -- By default, Cert Manager creates Kubernetes TLS secrets with the standard key names:
                  --     tls.crt – the certificate
                  --     tls.key – the private key
                  --     ca.crt – the CA certificate (only if available)
                -- These key names cannot be customized directly by Cert Manager. 
                -- But we can control how they're mounted in the pod by using Kubernetes volumeMounts and subPath tricks.
                , volumeMounts = Some [
                    kubernetes.VolumeMount::{
                      name = values.mtls.cassini.secretName
                      , mountPath = "/etc/cassini/tls/ca_certificate.pem"
                      , subPath = Some "ca.crt"
                      , readOnly = Some True
                    }
                  , kubernetes.VolumeMount::{
                      name = values.mtls.cassini.secretName
                      , mountPath = "/etc/cassini/tls/server_polar_certificate.pem"
                      , subPath = Some "tls.crt"
                      , readOnly = Some True
                    }
                  , kubernetes.VolumeMount::{
                      name = values.mtls.cassini.secretName
                      , mountPath = "/etc/cassini/tls/server_polar_key.pem"
                      , subPath = Some "tls.key"
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