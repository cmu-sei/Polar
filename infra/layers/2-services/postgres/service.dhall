-- infra/layers/2-services/postgres/service.dhall
--
-- ClusterIP only -- Postgres is never reached from outside the cluster;
-- Keycloak talks to it over in-cluster DNS.

let kubernetes = ../../../schema/kubernetes.dhall

let render =
      \(v : { namespace : Text, name : Text }) ->
        kubernetes.Service::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some v.name
          , namespace = Some v.namespace
          }
        , spec = Some kubernetes.ServiceSpec::{
          , selector = Some (toMap { name = v.name })
          , ports    = Some
            [ kubernetes.ServicePort::{
              , name       = Some "postgres"
              , targetPort = Some (kubernetes.NatOrString.Nat 5432)
              , port       = 5432
              }
            ]
          }
        }

in render
