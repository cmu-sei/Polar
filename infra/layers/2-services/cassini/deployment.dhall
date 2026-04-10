-- infra/layers/2-services/cassini/deployment.dhall
--
-- Cassini Deployment and Service.
-- Pure function: receives a values record + jaegerDNSName, produces resources.

let k8s       = ../../../schema/kubernetes.dhall
let Constants = ../../../schema/constants.dhall

let render =
      \(v :
          { name            : Text
          , image           : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , ports           : { http : Natural, tcp : Natural }
          , jaegerDNSName   : Text
          }
      ) ->

        let environment =
              [ k8s.EnvVar::{ name = "TLS_CA_CERT",          value = Some Constants.mtls.caCertPath  }
              , k8s.EnvVar::{ name = "TLS_SERVER_CERT_CHAIN", value = Some Constants.mtls.certPath    }
              , k8s.EnvVar::{ name = "TLS_SERVER_KEY",        value = Some Constants.mtls.keyPath     }
              , k8s.EnvVar::{
                , name  = "CASSINI_BIND_ADDR"
                , value = Some "0.0.0.0:${Natural/show v.ports.tcp}"
                }
              , k8s.EnvVar::{ name = "JAEGER_OTLP_ENDPOINT",  value = Some v.jaegerDNSName            }
              ]

        let volumes =
              [ k8s.Volume::{
                , name   = Constants.CassiniServerCertificateSecret
                , secret = Some k8s.SecretVolumeSource::{
                  , secretName = Some Constants.CassiniServerCertificateSecret
                  }
                }
              ]

        let volumeMounts =
              [ k8s.VolumeMount::{
                , name      = Constants.CassiniServerCertificateSecret
                , mountPath = Constants.tlsPath
                , readOnly  = Some True
                }
              ]

        let deployment =
              k8s.Deployment::{
              , metadata = k8s.ObjectMeta::{
                , name      = Some v.name
                , namespace = Some Constants.PolarNamespace
                }
              , spec = Some k8s.DeploymentSpec::{
                , selector = k8s.LabelSelector::{
                  , matchLabels = Some (toMap { name = v.name })
                  }
                , replicas = Some 1
                , template = k8s.PodTemplateSpec::{
                  , metadata = Some k8s.ObjectMeta::{
                    , name   = Some v.name
                    , labels = Some [ { mapKey = "name", mapValue = v.name } ]
                    }
                  , spec = Some k8s.PodSpec::{
                    , imagePullSecrets = Some v.imagePullSecrets
                    , containers =
                      [ k8s.Container::{
                        , name            = "cassini"
                        , image           = Some v.image
                        , imagePullPolicy = Some v.imagePullPolicy
                        , securityContext = Some k8s.SecurityContext::{
                          , runAsGroup   = Some 1000
                          , runAsNonRoot = Some True
                          , runAsUser    = Some 1000
                          , capabilities = Some k8s.Capabilities::{ drop = Some [ "ALL" ] }
                          }
                        , env          = Some environment
                        , ports        = Some
                          [ k8s.ContainerPort::{ containerPort = v.ports.tcp  }
                          , k8s.ContainerPort::{ containerPort = v.ports.http }
                          ]
                        , volumeMounts = Some volumeMounts
                        }
                      ]
                    , volumes = Some volumes
                    }
                  }
                }
              }

        let service =
              k8s.Service::{
              , metadata = k8s.ObjectMeta::{
                , name      = Some Constants.cassiniService.name
                , namespace = Some Constants.PolarNamespace
                }
              , spec = Some k8s.ServiceSpec::{
                , selector = Some (toMap { name = v.name })
                , type     = Some Constants.cassiniService.type
                , ports    = Some
                  [ k8s.ServicePort::{
                    , name       = Some "cassini-tcp"
                    , targetPort = Some (k8s.NatOrString.Nat v.ports.tcp)
                    , port       = v.ports.tcp
                    }
                  , k8s.ServicePort::{
                    , name       = Some "cassini-http"
                    , targetPort = Some (k8s.NatOrString.Nat v.ports.http)
                    , port       = v.ports.http
                    }
                  ]
                }
              }

        in  [ k8s.Resource.Service    service
            , k8s.Resource.Deployment deployment
            ]

in render
