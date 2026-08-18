-- infra/layers/2-services/keycloak/service.dhall
--
-- NodePort -- same shape as jaeger: one Service satisfies both in-cluster
-- reachability (cert-issuer -> Keycloak for JWKS/discovery) and
-- host-external reachability (an admins browser reaching the login UI).

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v : { name : Text, serviceType : Text, ports : { http : Natural } }) ->
        kubernetes.Service::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some "${v.name}-svc"
          , namespace = Some Constants.AuthNamespace
          }
        , spec = Some kubernetes.ServiceSpec::{
          , selector = Some (toMap { name = v.name })
          , type     = Some v.serviceType
          , ports    = Some
            [ kubernetes.ServicePort::{
              , name       = Some "http"
              , targetPort = Some (kubernetes.NatOrString.Nat v.ports.http)
              , port       = v.ports.http
              }
            ]
          }
        }

in render
