let values = ../values-active.dhall

in  if values.isLocal
    then
      [ { apiVersion = "storage.k8s.io/v1"
        , kind = "StorageClass"
        , metadata.name = "managed-csi"
        , provisioner = "rancher.io/local-path"
        , volumeBindingMode = "WaitForFirstConsumer"
        , reclaimPolicy = "Delete"
        }
      ]
    else
      [] : List { apiVersion : Text, kind : Text, metadata : { name : Text }, provisioner : Text, volumeBindingMode : Text, reclaimPolicy : Text }
