-- infra/layers/2-services/jaeger/deployment.dhall
--
-- Jaeger Deployment and Service.
-- Pure function: receives a values record, produces resources.

let k8s       = ../../../schema/kubernetes.dhall
let Constants = ../../../schema/constants.dhall

let render =
      \(v : { name           : Text
            , image          : Text
            , imagePullPolicy : Text
            , serviceType    : Text
            , ports          : { http : Natural, traces : Natural }
            }) ->

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
                    , containers =
                      [ k8s.Container::{
                        , name            = v.name
                        , image           = Some v.image
                        , imagePullPolicy = Some v.imagePullPolicy
                        , env             = Some
                          [ k8s.EnvVar::{
                            , name  = "COLLECTOR_OTLP_ENABLED"
                            , value = Some "true"
                            }
                          ]
                        , ports = Some
                          [ k8s.ContainerPort::{ containerPort = v.ports.traces }
                          , k8s.ContainerPort::{ containerPort = v.ports.http   }
                          ]
                        }
                      ]
                    }
                  }
                }
              }

        let service =
              k8s.Service::{
              , metadata = k8s.ObjectMeta::{
                , name      = Some "jaeger-svc"
                , namespace = Some Constants.PolarNamespace
                }
              , spec = Some k8s.ServiceSpec::{
                , selector = Some (toMap { name = v.name })
                , type     = Some v.serviceType
                , ports    = Some
                  [ k8s.ServicePort::{
                    , name       = Some "traces"
                    , targetPort = Some (k8s.NatOrString.Nat v.ports.traces)
                    , port       = v.ports.traces
                    }
                  , k8s.ServicePort::{
                    , name       = Some "http"
                    , targetPort = Some (k8s.NatOrString.Nat v.ports.http)
                    , port       = v.ports.http
                    }
                  ]
                }
              }

        in  [ k8s.Resource.Deployment deployment
            , k8s.Resource.Service    service
            ]

in render
