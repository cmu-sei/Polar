-- infra/layers/2-services/cert-issuer/pvc.dhall
-- PVC for CA materials. Must survive pod restarts — emptyDir is not acceptable.

let Constants = ../../../schema/constants.dhall

let render =
      \(v : { name : Text, caVolumeName : Text, caStorageClass : Text, caStorageSize : Text }) ->
        { apiVersion = "v1"
        , kind       = "PersistentVolumeClaim"
        , metadata   =
          { name      = "${v.name}-ca"
          , namespace = Constants.PolarNamespace
          }
        , spec =
          { accessModes      = [ "ReadWriteOnce" ]
          , storageClassName = v.caStorageClass
          , resources.requests.storage = v.caStorageSize
          }
        }

in render
