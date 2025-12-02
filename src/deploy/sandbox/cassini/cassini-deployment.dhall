let kubernetes = ../../types/kubernetes.dhall

let values = ../values.dhall

let containers =
      [ kubernetes.Container::{
        , name = "cassini"
        , image = Some values.cassini.image
        , securityContext = Some kubernetes.SecurityContext::{
          , runAsGroup = Some 1000
          , runAsNonRoot = Some True
          , runAsUser = Some 1000
          , capabilities = Some kubernetes.Capabilities::{
            , drop = Some [ "ALL" ]
            }
          }
        , env = Some values.cassini.environment
        , ports = Some
          [ kubernetes.ContainerPort::{ containerPort = values.cassini.port } ]
        , volumeMounts = Some values.cassini.volumeMounts
        }
      ]

let spec =
      kubernetes.PodSpec::{
      , imagePullSecrets = Some values.sandboxRegistry.imagePullSecrets
      , containers
      , volumes = Some values.cassini.volumes
      }

in  kubernetes.Deployment::{
    , metadata = kubernetes.ObjectMeta::{
      , name = Some values.cassini.name
      , namespace = Some values.namespace
      }
    , spec = Some kubernetes.DeploymentSpec::{
      , selector = kubernetes.LabelSelector::{
        , matchLabels = Some (toMap { name = values.cassini.name })
        }
      , replicas = Some 1
      , template = kubernetes.PodTemplateSpec::{
        , metadata = Some kubernetes.ObjectMeta::{
          , name = Some values.cassini.name
          , labels = Some
            [ { mapKey = "name", mapValue = values.cassini.name } ]
          }
        , spec = Some spec
        }
      }
    }
