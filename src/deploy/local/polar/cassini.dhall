let kubernetes = ../../types/kubernetes.dhall

let Constants = ../../types/constants.dhall

let values = ../values.dhall

let serviceSpec =
      kubernetes.ServiceSpec::{
      , selector = Some (toMap { name = values.cassini.name })
      , type = Some Constants.cassiniService.type
      , ports = Some
        [ kubernetes.ServicePort::{
          , name = Some "cassini-tcp"
          , targetPort = Some
              (kubernetes.NatOrString.Nat values.cassini.ports.tcp)
          , port = values.cassini.ports.tcp
          }
        , kubernetes.ServicePort::{
          , name = Some "cassini-http"
          , targetPort = Some
              (kubernetes.NatOrString.Nat values.cassini.ports.http)
          , port = values.cassini.ports.http
          }
        ]
      }

let cassiniService =
      kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.cassiniService.name
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some kubernetes.ServiceSpec::serviceSpec
      }

let environment =
      [ kubernetes.EnvVar::{
        , name = "TLS_CA_CERT"
        , value = Some Constants.mtls.caCertPath
        }
      , kubernetes.EnvVar::{
        , name = "TLS_SERVER_CERT_CHAIN"
        , value = Some Constants.mtls.certPath
        }
      , kubernetes.EnvVar::{
        , name = "TLS_SERVER_KEY"
        , value = Some Constants.mtls.keyPath
        }
      , kubernetes.EnvVar::{
        , name = "CASSINI_BIND_ADDR"
        , value = Some "0.0.0.0:${Natural/show values.cassini.ports.tcp}"
        }
      ]

let volumes =
      [ kubernetes.Volume::{
        , name = Constants.CassiniServerCertificateSecret
        , secret = Some kubernetes.SecretVolumeSource::{
          , secretName = Some Constants.CassiniServerCertificateSecret
          }
        }
      ]

let volumeMounts =
      [ kubernetes.VolumeMount::{
        , name = Constants.CassiniServerCertificateSecret
        , mountPath = Constants.tlsPath
        , readOnly = Some True
        }
      ]

let containers =
      [ kubernetes.Container::{
        , name = "cassini"
        , image = Some values.cassini.image
        , imagePullPolicy = Some "Never"
        , securityContext = Some kubernetes.SecurityContext::{
          , runAsGroup = Some 1000
          , runAsNonRoot = Some True
          , runAsUser = Some 1000
          , capabilities = Some kubernetes.Capabilities::{
            , drop = Some [ "ALL" ]
            }
          }
        , env = Some environment
        , ports = Some
          [ kubernetes.ContainerPort::{
            , containerPort = values.cassini.ports.tcp
            }
          , kubernetes.ContainerPort::{
            , containerPort = values.cassini.ports.http
            }
          ]
        , volumeMounts = Some volumeMounts
        }
      ]

let spec = kubernetes.PodSpec::{ containers, volumes = Some volumes }

let deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.cassini.name
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = values.cassini.name })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name = Some values.cassini.name
            , labels = Some
              [ { mapKey = "name", mapValue = values.cassini.name } ]
            }
          , spec = Some spec
          }
        }
      }




in  [ kubernetes.Resource.Service cassiniService
    , kubernetes.Resource.Deployment deployment
    ]
