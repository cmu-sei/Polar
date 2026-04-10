-- infra/layers/1-platform/infra/storage/storage-class.dhall
--
-- Defines the managed-csi StorageClass that neo4j PVCs bind against.
-- On local, this is backed by rancher local-path-provisioner.
-- On sandbox/AKS, managed-csi is provided natively by the platform.
--
-- Whether this file is rendered at all is a target decision in target.nu,
-- not a conditional in this file. The isLocal branch is gone.

[ { apiVersion         = "storage.k8s.io/v1"
  , kind               = "StorageClass"
  , metadata.name      = "managed-csi"
  , provisioner        = "rancher.io/local-path"
  , volumeBindingMode  = "WaitForFirstConsumer"
  , reclaimPolicy      = "Delete"
  }
]
