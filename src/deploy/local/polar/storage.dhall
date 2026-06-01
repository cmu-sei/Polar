{-
  local/storage.dhall — storage class shim for local cluster development.

  Most local Kubernetes clusters ships with a `local-path` provisioner
  but none of the cloud storage classes referenced in the deployment
  manifests (managed-csi, managed-csi-premium, etc.). This file
  creates a managed-csi StorageClass backed by local-path so PVC
  definitions work unchanged between local and cloud environments.

  Apply before neo4j.dhall or any manifest that creates PVCs:
    kubectl apply -f storage.dhall

  Do NOT apply this in a real AKS cluster — AKS ships with a real
  managed-csi StorageClass and applying this would replace it with
  a local-path-backed one, breaking actual disk provisioning.
-}
let kubernetes = ../../types/kubernetes.dhall

let managedCsiShim =
      { apiVersion = "storage.k8s.io/v1"
      , kind = "StorageClass"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "managed-csi"
        , annotations = Some
          [ { mapKey = "storageclass.kubernetes.io/is-default-class"
            , mapValue = "false"
            }
          ]
        }
      , provisioner = "rancher.io/local-path"
      , reclaimPolicy = Some "Delete"
      , volumeBindingMode = Some "WaitForFirstConsumer"
      }

in  managedCsiShim
