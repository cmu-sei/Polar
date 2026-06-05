-- infra/layers/2-services/neo4j/pvcs.dhall
--
-- Neo4j PersistentVolumeClaims: data and logs.
-- The certs PVC has been removed — TLS certs are now issued by the
-- cert-issuer and written to an emptyDir by the cert-client init container.

let kubernetes = ../../../schema/kubernetes.dhall

let VolumeSpec =
      { name             : Text
      , storageClassName : Optional Text
      , storageSize      : Text
      , mountPath        : Text
      }

let makePVC =
      \(namespace : Text) ->
      \(v : VolumeSpec) ->
        kubernetes.PersistentVolumeClaim::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some v.name
          , namespace = Some namespace
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

let render =
      \(v :
          { namespace : Text
          , volumes   :
            { data : VolumeSpec
            , logs : VolumeSpec
            , certs : VolumeSpec
            }
          }
      ) ->
        [ kubernetes.Resource.PersistentVolumeClaim (makePVC v.namespace v.volumes.data)
        , kubernetes.Resource.PersistentVolumeClaim (makePVC v.namespace v.volumes.logs)
        , kubernetes.Resource.PersistentVolumeClaim (makePVC v.namespace v.volumes.certs)
        ]

in render
