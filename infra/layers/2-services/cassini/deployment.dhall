-- infra/layers/2-services/cassini/deployment.dhall
--
-- Cassini Deployment and Service.
-- Pure function: receives a values record + jaegerDNSName, produces resources.

let k8s       = ../../../schema/kubernetes.dhall
let Constants = ../../../schema/constants.dhall
let functions = ../../../schema/functions.dhall

let render =
      \(v :
          { name             : Text
          , image            : Text
          , imagePullPolicy  : Text
          , imagePullSecrets : List { name : Optional Text }
          , certClientImage  : Text
          , certIssuerUrl    : Text
          , saTokenAudience  : Text
          , ports            : { http : Natural, tcp : Natural }
          , jaegerDNSName    : Text
          }
      ) ->
        let environment =
              [ k8s.EnvVar::{ name = "TLS_CA_CERT",           value = Some Constants.mtls.caCertPath }
              , k8s.EnvVar::{ name = "TLS_SERVER_CERT_CHAIN", value = Some Constants.mtls.certPath   }
              , k8s.EnvVar::{ name = "TLS_SERVER_KEY",        value = Some Constants.mtls.keyPath    }
              , k8s.EnvVar::{ name = "CASSINI_BIND_ADDR",     value = Some "0.0.0.0:${Natural/show v.ports.tcp}" }
              , k8s.EnvVar::{ name = "JAEGER_OTLP_ENDPOINT",  value = Some v.jaegerDNSName           }
              ]

        let certInitContainer =
              k8s.Container::{
              , name  = "cert-client"
              , image = Some v.certClientImage
              , imagePullPolicy = Some "IfNotPresent"
              , args  = Some
                [ "--cert-issuer-url", v.certIssuerUrl
                , "--token-path",      "/workspace/token"
                , "--cert-dir",        Constants.tlsPath
                , "--cert-type",       "server"
                , "--extra-san",       Constants.cassiniDNSName
                ]
              , securityContext = Some Constants.DropAllCapSecurityContext
              , volumeMounts = Some
                [ k8s.VolumeMount::{ name = Constants.certVolumeName,    mountPath = Constants.tlsPath }
                , k8s.VolumeMount::{ name = Constants.saTokenVolumeName, mountPath = "/workspace"      }
                ]
              }

        let deployment =
              k8s.Deployment::{
              , metadata = k8s.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace }
              , spec = Some k8s.DeploymentSpec::{
                , selector = k8s.LabelSelector::{ matchLabels = Some (toMap { name = v.name }) }
                , replicas = Some 1
                , template = k8s.PodTemplateSpec::{
                  , metadata = Some k8s.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                  , spec = Some k8s.PodSpec::{
                    , imagePullSecrets = Some v.imagePullSecrets
                    , volumes = Some
                      [ Constants.certEmptyDirVolume
                      , Constants.saTokenVolume v.saTokenAudience
                      ]
                    , initContainers = Some [ certInitContainer ]
                    , containers =
                      [ k8s.Container::{
                        , name            = "cassini"
                        , image           = Some v.image
                        , imagePullPolicy = Some "IfNotPresent"
                        , securityContext = Some Constants.DropAllCapSecurityContext
                        , env             = Some environment
                        , ports           = Some
                          [ k8s.ContainerPort::{ containerPort = v.ports.tcp  }
                          , k8s.ContainerPort::{ containerPort = v.ports.http }
                          ]
                        , volumeMounts = Some
                          [ k8s.VolumeMount::{ name = Constants.certVolumeName, mountPath = Constants.tlsPath, readOnly = Some True } ]
                        }
                      ]
                    }
                  }
                }
              }

        let service =
              k8s.Service::{
              , metadata = k8s.ObjectMeta::{ name = Some Constants.cassiniService.name, namespace = Some Constants.PolarNamespace }
              , spec = Some k8s.ServiceSpec::{
                , selector = Some (toMap { name = v.name })
                , type     = Some Constants.cassiniService.type
                , ports    = Some
                  [ k8s.ServicePort::{ name = Some "cassini-tcp",  targetPort = Some (k8s.NatOrString.Nat v.ports.tcp),  port = v.ports.tcp  }
                  , k8s.ServicePort::{ name = Some "cassini-http", targetPort = Some (k8s.NatOrString.Nat v.ports.http), port = v.ports.http }
                  ]
                }
              }

        in  [ k8s.Resource.Service service, k8s.Resource.Deployment deployment ]

in render
