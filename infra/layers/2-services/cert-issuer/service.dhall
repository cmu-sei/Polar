-- infra/layers/2-services/cert-issuer/service.dhall

let k8s       = ../../../schema/kubernetes.dhall
let Constants = ../../../schema/constants.dhall

let render =
      \(v : { name : Text, port : Natural }) ->
        k8s.Service::{
        , metadata = k8s.ObjectMeta::{
          , name      = Some v.name
          , namespace = Some Constants.PolarNamespace
          }
        , spec = Some k8s.ServiceSpec::{
          , selector = Some (toMap { name = v.name })
          , type     = Some "ClusterIP"
          , ports    = Some
            [ k8s.ServicePort::{
              , name       = Some "http"
              , port       = v.port
              , targetPort = Some (k8s.NatOrString.Nat v.port)
              }
            ]
          }
        }

in render
