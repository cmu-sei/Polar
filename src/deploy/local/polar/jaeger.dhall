let k8s = ../../types/kubernetes.dhall
let Constants = ../../types/constants.dhall

let Deployment = k8s.Deployment

let Service = k8s.Service

let Metadata = k8s.ObjectMeta

let Container = k8s.Container

let ContainerPort = k8s.ContainerPort

let ServicePort = k8s.ServicePort

let LabelSelector = k8s.LabelSelector

let PodSpec = k8s.PodSpec

let PodTemplateSpec = k8s.PodTemplateSpec

let DeploymentSpec = k8s.DeploymentSpec

let ServiceSpec = k8s.ServiceSpec

let JAEGER = "jaeger"

let portConfig = { http = 16686 , traces = 4318 }

let ports = Some [
    ContainerPort::{ containerPort = portConfig.traces }
    ,ContainerPort::{ containerPort = portConfig.http }

]

let containers =
  [
    k8s.Container::{
    , name = JAEGER
    , image = Some "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
    , env = Some [ k8s.EnvVar::{ name = "COLLECTOR_OTLP_ENABLED", value = Some "true" } ]
    , ports
    }
  ]

let spec
  = k8s.PodSpec::{
    containers
  }

let deployment = k8s.Deployment::{
    , metadata = k8s.ObjectMeta::{
    , name = Some JAEGER
    , namespace = Some Constants.PolarNamespace
    }
    , spec = Some k8s.DeploymentSpec::{
    , selector = k8s.LabelSelector::{
        , matchLabels = Some (toMap { name = JAEGER })
        }
    , replicas = Some 1
    , template = k8s.PodTemplateSpec::{
        , metadata = Some k8s.ObjectMeta::{
        , name = Some JAEGER
        , labels = Some
            [ { mapKey = "name", mapValue = JAEGER } ]
        }
        , spec = Some spec
        }
    }
}

-- -----------------------------------------------------------
-- Jaeger Service (ClusterIP + NodePort for UI)
-- -----------------------------------------------------------

let serviceSpec =
      k8s.ServiceSpec::{
      , selector = Some (toMap { name = JAEGER })
      , type = Some "NodePort"
      , ports = Some
        [
        k8s.ServicePort::{
          , name = Some "traces"
          , targetPort = Some
              (k8s.NatOrString.Nat portConfig.traces)
              , port = portConfig.traces
          }
        , k8s.ServicePort::{
          , name = Some "http"
          , targetPort = Some
              (k8s.NatOrString.Nat portConfig.http)
              , port = portConfig.http
          }
        ]
      }

let service =
      k8s.Service::{
      , metadata = k8s.ObjectMeta::{
        , name = Some "jaeger-svc"
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some k8s.ServiceSpec::serviceSpec
      }


in [ k8s.Resource.Deployment deployment, k8s.Resource.Service service ]
