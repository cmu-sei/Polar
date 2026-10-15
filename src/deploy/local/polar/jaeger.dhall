-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

let k8s = ../../types/kubernetes.dhall
let Constants = ../../types/lib-constants.dhall

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
    , imagePullPolicy = Some "IfNotPresent"
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
    , namespace = Some Constants.polarNamespace
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
        , namespace = Some Constants.polarNamespace
        }
      , spec = Some k8s.ServiceSpec::serviceSpec
      }


in [ k8s.Resource.Deployment deployment, k8s.Resource.Service service ]
