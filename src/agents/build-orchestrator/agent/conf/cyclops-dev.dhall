-- cyclops-dev.dhall
--
-- Orchestrator config for a laptop: the orchestrator runs on the host, jobs
-- run in the `cyclops-jobs` namespace from build-orchestrator/deploy/dev.dhall.
-- Render with:
--   dhall-to-yaml --file cyclops-dev.dhall --output cyclops.yaml
--
-- Sized to fit that namespace's LimitRange (at most 1 CPU / 2Gi per container)
-- and quota (4 job pods). The CI-sized defaults are refused there on purpose.
let orchestrator = ./orchestrator.dhall

in  orchestrator.OrchestratorConfig::{
    , backend =
      { driver = "kubernetes"
      , command = [] : List Text
        -- The broker runs on the host; pods reach it through OrbStack's
        -- host alias. The dev certs are issued for the name "cassini".
      , job_env =
        [ { name = "BROKER_ADDR", value = "host.docker.internal:8080" }
        , { name = "CASSINI_SERVER_NAME", value = "cassini" }
        ]
      , kubernetes = Some orchestrator.KubernetesBackendConfig::{
        , namespace = "cyclops-jobs"
        , resources = Some
          { cpu_limit = Some "1"
          , memory_limit = Some "1Gi"
          , cpu_request = Some "100m"
          , memory_request = Some "256Mi"
          }
        , workspace_size_limit = Some "1Gi"
          -- Behind a TLS-inspecting proxy (e.g. Zscaler), create the
          -- `proxy-ca` Secret (see the README) and set this to Some "proxy-ca".
          -- If set, the Secret must exist, or job containers can't start.
        , ca_bundle_secret = None Text
        }
      , podman = None orchestrator.PodmanBackendConfig.Type
      }
    , storage = orchestrator.StorageConfig::{
      -- Dev object store (deploy/dev.dhall), via its NodePort.
      , endpoint_url = "http://localhost:30900"
      , access_key = "cyclops-dev"
      , secret_key = "cyclops-dev-secret"
      , bucket = "cyclops-build-artifacts"
      }
      -- A cheap stand-in "pipeline" for git builds: list the clone. Replace
      -- with polar-ci and `command = ["nu", "scripts/pipeline/ci.nu"]` for a
      -- real build.
    , repo_mappings =
      [ { repo_url = "https://github.com/cmu-sei/Polar.git"
        , pipeline_image = Some "alpine:3.20"
        }
      ]
    }
