{ apiVersion = "storage.k8s.io/v1"
, kind = "StorageClass"
, metadata =
  { annotations.`storageclass.kubernetes.io/is-default-class` = "true"
  , name = "standard"
  }
, provisioner = "kubernetes.io/azure-disk"
, volumeBindingMode = "Immediate"
}
