let k8s = ../../types/kubernetes.dhall
let Constants = ../../types/constants.dhall
let values = ../values.dhall

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

let ports = Some [
    ContainerPort::{ containerPort = values.jaeger.ports.traces }
    ,ContainerPort::{ containerPort = values.jaeger.ports.http }
]

let containers =
  [
    k8s.Container::{
    , name = JAEGER
    , image = Some values.jaeger.image
    , env = Some [ k8s.EnvVar::{ name = "COLLECTOR_OTLP_ENABLED", value = Some "true" } ]
    , ports
    }
  ]

let spec
  = k8s.PodSpec::{
    , imagePullSecrets = Some Constants.SandboxRegistry.imagePullSecrets
    , containers
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
              (k8s.NatOrString.Nat values.jaeger.ports.traces)
              , port = values.jaeger.ports.traces
          }
        , k8s.ServicePort::{
          , name = Some "http"
          , targetPort = Some
              (k8s.NatOrString.Nat values.jaeger.ports.http)
              , port = values.jaeger.ports.http
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
