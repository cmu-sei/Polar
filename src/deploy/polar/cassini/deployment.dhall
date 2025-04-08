let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall


let 
    deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        name = Some values.cassini.name
        , namespace = Some values.namespace
       }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = values.cassini.name })
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
                name = Some values.cassini.name
            ,   labels = Some [ { mapKey = "name", mapValue = values.cassini.name } ]
            }
          , spec = Some kubernetes.PodSpec::{
            , containers =
              [ 
                kubernetes.Container::{
                , name = "cassini"
                , image = Some values.cassini.image
                , env = Some values.cassini.environment 
                , ports = Some
                  [ kubernetes.ContainerPort::{ containerPort = values.cassini.port } ]
                , volumeMounts = Some values.cassini.volumeMounts

                },
              ]
            , volumes = Some values.cassini.volumes
            }
          }
        }
      }
    
in  deployment