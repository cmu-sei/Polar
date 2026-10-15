-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

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
