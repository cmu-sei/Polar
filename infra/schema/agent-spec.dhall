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
      -- neither       -> an emptyDir
      { name : Text, secret : Optional Text, audience : Optional Text }

let MountEntry = { name : Text, path : Text, read_only : Bool }

let InitMountEntry = { name : Text, path : Text }

let Probe =
      { initial_delay_secs : Natural
      , period_secs : Natural
      , failure_threshold : Natural
      , timeout_secs : Natural
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
      , backoff_limit : Natural
      , ttl_after_finished_secs : Natural
      , probe : Probe
      , termination_grace_secs : Natural
      , proxy_ca_cert : Optional Text
      , needs : List Text
      , env : List EnvEntry
      , volumes : List VolumeEntry
      , mounts : List MountEntry
      , init_mounts : List InitMountEntry
      , dep_checks : List Text
      , certs : List Text
      }

in  { Spec, EnvEntry, VolumeEntry, MountEntry, InitMountEntry, Probe }
