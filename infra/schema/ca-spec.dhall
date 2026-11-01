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

-- infra/schema/ca-spec.dhall
--
-- The record `infra/resolve.nu json <workload>` produces for a workload of kind "ca" (cert-issuer alone:
-- docs/architecture/cert-issuer-isolation.md, step 1c), as a Dhall type, for schema/ca.dhall. It is the agent's
-- (agent-spec.dhall) without what polar-init is given, since the issuer has no polar-init, and with what only its pod
-- has: a Secret's file mode and the pod's group. The entries it shares with the agent's are the agent's own types.

let A = ./agent-spec.dhall

let VolumeEntry =
      -- As the agent's (agent-spec.dhall), with the mode of a Secret volume's files: none means Kubernetes' default.
      { name : Text
      , secret : Optional Text
      , audience : Optional Text
      , config_map : Optional Text
      , claim : Optional Text
      , items : List { key : Text, path : Text }
      , mode : Optional Natural
      }

let Spec =
      { name : Text
      , kind : Text
      , namespace : Text
      , image : Text
      , pull_policy : Text
      , pull_secrets : List { name : Optional Text }
      , sa_token_audience : Text
      , sa_token_volume : Text
      , sa_token_path : Text
      , sa_token_expiration_secs : Natural
      , service_account : Text
      , sa_token_secret : Optional Text
      , automount_sa_token : Optional Bool
      , restart_policy : Text
      -- A run's Job (the `run` kind), or none: a Deployment of one that restarts in place.
      , job : Optional { backoff_limit : Natural, ttl_after_finished_secs : Natural }
      -- "one", or "many": many pods of the one identity at once, each an instance (authority-model.md, "Multi-instance
      -- workloads"), whose object is marked so.
      , instances : < one | many >
      , probe : A.Probe
      , termination_grace_secs : Natural
      , fs_group : Optional Natural
      , needs : List Text
      , env : List A.EnvEntry
      , volumes : List VolumeEntry
      , mounts : List A.MountEntry
      , ports : List Natural
      , placement : Optional { key : Text, pool : Text }
      , memory : Optional { request : Text, limit : Text }
      }

in  { Spec, VolumeEntry }
