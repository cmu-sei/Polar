let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/master/package.dhall
        sha256:263ee915ef545f2d771fdcd5cfa4fbb7f62772a861b5c197f998e5b71219112c

let values = ../values.dhall

let spec =
      { selector = Some (toMap { name = values.neo4j.name })
      , type = Some "ClusterIP"
      , ports = Some
        [ kubernetes.ServicePort::{
            name = Some "http-ui"
          , targetPort = Some (kubernetes.NatOrString.Nat values.neo4jPorts.https)
          , port = values.neo4jPorts.https
          },
          kubernetes.ServicePort::{
            name = Some "bolt"
          , targetPort = Some (kubernetes.NatOrString.Nat values.neo4jPorts.bolt)
          , port = values.neo4jPorts.bolt
          }
        ]
      }

let service
    : kubernetes.Service.Type
    = kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some values.neo4j.service.name
        , namespace = Some values.neo4j.namespace
        }
      , spec = Some kubernetes.ServiceSpec::spec
      }
in  service
