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

-- infra/schema/agent-spec.dhall
--
-- The record `infra/resolve.nu json <workload>` produces for a workload of
-- kind "agent", as a Dhall type. render-catalog.nu hands that JSON to
-- `json-to-dhall` with `(./agent-spec.dhall).Spec` as the schema, so a field
-- the resolver emits that is missing here (or the reverse) fails at render
-- time with a type error, not as a wrong manifest.
--
-- Everything is a plain Text, Natural, Bool, Optional or List: no unions,
-- because Dhall cannot compare Text and a JSON union encoding is fragile.
-- "Which kind of volume is this" is therefore read off which Optional field
-- is set, in agent.dhall.

let EnvEntry =
      -- Exactly one of `value` or `secret` is set. `key` goes with `secret`.
      { name : Text
      , value : Optional Text
      , secret : Optional Text
      , key : Optional Text
      }

let VolumeEntry =
      -- secret set    -> a Secret volume
      -- audience set  -> a projected ServiceAccount token with that audience
      -- config_map set -> a ConfigMap volume, of that ConfigMap
      -- claim set     -> a persistent volume claim, of that name
      -- none of them  -> an emptyDir
      { name : Text
      , secret : Optional Text
      , audience : Optional Text
      , config_map : Optional Text
      , claim : Optional Text
      -- the keys of a Secret volume to put in it, and the file each is: none means all of them
      , items : List { key : Text, path : Text }
      }

let MountEntry = { name : Text, path : Text, read_only : Bool }

let InitMountEntry = { name : Text, path : Text }

let Probe =
      { initial_delay_secs : Natural
      , period_secs : Natural
      , failure_threshold : Natural
      , timeout_secs : Natural
      }

-- The custody daemon's place in the pod (docs/architecture/key-custody.md, step 4): its image, its socket's volume
-- and path, the CA it judges certificates by and that CA's volume, its UID and the group it shares the socket in, and
-- the UIDs it serves.
let Custody =
      { image : Text
      , volume : Text
      , dir : Text
      , socket : Text
      , ca_cert : Text
      , ca_dir : Text
      , ca_volume : Text
      , daemon_uid : Natural
      , group : Natural
      , installer_uid : Natural
      , workload_uids : Text
      }

let Spec =
      { name : Text
      , kind : Text
      , namespace : Text
      , image : Text
      , pull_policy : Text
      , pull_secrets : List { name : Optional Text }
      , polar_init_image : Text
      , cert_issuer_url : Text
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
      , probe : Probe
      , termination_grace_secs : Natural
      , proxy_ca_cert : Optional Text
      , needs : List Text
      , env : List EnvEntry
      , volumes : List VolumeEntry
      , mounts : List MountEntry
      , init_mounts : List InitMountEntry
      , init_env : List EnvEntry
      , ports : List Natural
      , dep_checks : List Text
      , certs : List Text
      , custody : Optional Custody
      , placement : Optional { key : Text, pool : Text }
      }

in  { Spec, EnvEntry, VolumeEntry, MountEntry, InitMountEntry, Probe, Custody }
