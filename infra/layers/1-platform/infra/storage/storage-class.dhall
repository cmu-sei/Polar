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
