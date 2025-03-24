let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/master/package.dhall
        sha256:263ee915ef545f2d771fdcd5cfa4fbb7f62772a861b5c197f998e5b71219112c

let spec =
      { selector = Some (toMap { name = "cassini-ip-svc" })
      , type = Some "ClusterIP"
      , ports = Some
        [ kubernetes.ServicePort::{
            name = Some "cassini-tcp"
          , targetPort = Some (kubernetes.NatOrString.Nat 8080)
          , port = 8080
          }
        ]
      }

let service
    : kubernetes.Service.Type
    = kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "cassini"
        , namespace = Some "polar"
        }
      , spec = Some kubernetes.ServiceSpec::spec
      }
in  service
