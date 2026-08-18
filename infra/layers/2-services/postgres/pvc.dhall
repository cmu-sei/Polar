-- infra/layers/2-services/postgres/pvc.dhall
--
-- Single data PersistentVolumeClaim for Postgres.

let kubernetes = ../../../schema/kubernetes.dhall

let render =
      \(v :
          { namespace        : Text
          , name             : Text
          , storageClassName : Optional Text
          , storageSize      : Text
          }
      ) ->
        [ kubernetes.Resource.PersistentVolumeClaim
            kubernetes.PersistentVolumeClaim::{
            , metadata = kubernetes.ObjectMeta::{
              , name      = Some "${v.name}-data"
              , namespace = Some v.namespace
              }
            , spec = Some kubernetes.PersistentVolumeClaimSpec::{
              , accessModes = Some [ "ReadWriteOnce" ]
              , resources   = Some kubernetes.VolumeResourceRequirements::{
                , requests = Some
                  [ { mapKey = "storage", mapValue = v.storageSize } ]
                }
              , storageClassName = v.storageClassName
              }
            }
        ]

in render
