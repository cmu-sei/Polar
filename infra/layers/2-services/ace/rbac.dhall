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

-- infra/layers/2-services/ace/rbac.dhall
--
-- ACE's dedicated ServiceAccount.
--
-- ACE only talks to Neo4j -- it never calls the Kubernetes API -- so
-- unlike kube-observer's rbac.dhall this needs no ClusterRole or
-- ClusterRoleBinding. The ServiceAccount exists solely to give ACE its
-- own workload identity for cert-issuer, distinct from `default` in the
-- polar namespace, so its one-year certificate lifetime override
-- (identityLifetimeOverrides) applies only to ACE and not to every other
-- workload sharing the default ServiceAccount.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v : { serviceAccountName : Text }) ->

        kubernetes.ServiceAccount::{
        , apiVersion = "v1"
        , kind       = "ServiceAccount"
        , metadata   = kubernetes.ObjectMeta::{
          , name      = Some v.serviceAccountName
          , namespace = Some Constants.PolarNamespace
          }
        , automountServiceAccountToken = Some False
        }

in render
