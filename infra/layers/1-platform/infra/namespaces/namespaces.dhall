-- infra/layers/1-platform/infra/namespaces/namespaces.dhall
--
-- Defines all namespaces required by the Polar stack.
-- Applied first — everything else depends on these existing.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall

in  [ kubernetes.Resource.Namespace
        kubernetes.Namespace::{
        , apiVersion = "v1"
        , kind       = "Namespace"
        , metadata   = kubernetes.ObjectMeta::{
          , name = Some Constants.PolarNamespace
          }
        }
    , kubernetes.Resource.Namespace
        kubernetes.Namespace::{
        , apiVersion = "v1"
        , kind       = "Namespace"
        , metadata   = kubernetes.ObjectMeta::{
          , name = Some Constants.GraphNamespace
          }
        }
    ]
