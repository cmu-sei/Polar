let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/master/package.dhall
        sha256:263ee915ef545f2d771fdcd5cfa4fbb7f62772a861b5c197f998e5b71219112c

let spec =
      { selector = Some (toMap { name = "neo4j" })
      , type = Some "NodePort"
      , ports = Some
        [ kubernetes.ServicePort::{
            name = Some "http-ui"
          , targetPort = Some (kubernetes.NatOrString.Nat 7474)
          , port = 7474
          , nodePort = Some 30074
          },
          kubernetes.ServicePort::{
            name = Some "bolt"
          , targetPort = Some (kubernetes.NatOrString.Nat 7687)
          , port = 7687
          , nodePort = Some 30087
          }
        ]
      }

let service
    : kubernetes.Service.Type
    = kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "neo4j"
        , namespace = Some "polar"
        }
      , spec = Some kubernetes.ServiceSpec::spec
      }
in  service
