-- infra/layers/2-services/neo4j/service.dhall
--
-- Neo4j Service. Type and ports are TLS-conditional.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v :
          { enableTls : Bool
          , ports     : { http : Natural, https : Natural, bolt : Natural }
          }
      ) ->
        kubernetes.Service::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some Constants.neo4jServiceName
          , namespace = Some Constants.GraphNamespace
          }
        , spec = Some kubernetes.ServiceSpec::{
          , selector = Some (toMap { name = "polar-neo4j" })
          , type     = Some (if v.enableTls then "LoadBalancer" else "NodePort")
          , ports    = Some
            ( if v.enableTls
              then
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
              else
                [ kubernetes.ServicePort::{
                  , name       = Some "http-ui"
                  , protocol   = Some "TCP"
                  , targetPort = Some (kubernetes.NatOrString.Nat v.ports.http)
                  , port       = v.ports.http
                  }
                , kubernetes.ServicePort::{
                  , name       = Some "bolt"
                  , protocol   = Some "TCP"
                  , targetPort = Some (kubernetes.NatOrString.Nat v.ports.bolt)
                  , port       = v.ports.bolt
                  }
                ]
            )
          }
        }

in render
