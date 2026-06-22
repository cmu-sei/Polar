-- infra/layers/2-services/neo4j/service.dhall
--
-- Neo4j Service. TLS is always enabled via cert-issuer.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v :
          { ports : { http : Natural, https : Natural, bolt : Natural }
          }
      ) ->
        kubernetes.Service::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some Constants.neo4jServiceName
          , namespace = Some Constants.GraphNamespace
          }
        , spec = Some kubernetes.ServiceSpec::{
          , selector = Some (toMap { name = "polar-neo4j" })
          , type     = Some "LoadBalancer"
          , ports    = Some
            [ kubernetes.ServicePort::{
              , name       = Some "https-ui"
              , protocol   = Some "TCP"
              , targetPort = Some (kubernetes.NatOrString.Nat v.ports.https)
              , port       = v.ports.https
              }
            , kubernetes.ServicePort::{
              , name       = Some "bolt"
              , protocol   = Some "TCP"
              , targetPort = Some (kubernetes.NatOrString.Nat v.ports.bolt)
              , port       = v.ports.bolt
              }
            ]
          }
        }

in render
