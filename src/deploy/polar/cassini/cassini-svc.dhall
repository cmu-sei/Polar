let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

let spec =
      { selector = Some (toMap { name = values.cassini.name })
      , type = Some "ClusterIP"
      , ports = Some
        [ kubernetes.ServicePort::{
            name = Some "cassini-tcp"
          , targetPort = Some (kubernetes.NatOrString.Nat values.cassini.port)
          , port = values.cassini.port
          }
        ]
      }

let service
    : kubernetes.Service.Type
    = kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.cassini.service.name
        , namespace = Some values.namespace
        }
      , spec = Some kubernetes.ServiceSpec::spec
      }
in  service