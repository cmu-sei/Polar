let orchestrator = ./orchestrator.dhall

in  orchestrator.OrchestratorConfig::{
    , backend =
    { driver = "podman"
    , command = ["cargo", "version" ]
    , job_env =
      [ { name = "BROKER_ADDR", value = "localhost:8080" }
      , { name = "CASSINI_SERVER_NAME", value = "localhost" }
      ]
    , kubernetes = None orchestrator.KubernetesBackendConfig.Type
    , podman = Some orchestrator.PodmanBackendConfig::{ socket_path = env:PODMAN_DEV_SOCKET as Text }
    }
    , storage = orchestrator.StorageConfig::{
      -- Dev object store (deploy/dev.dhall), via its NodePort.
      , endpoint_url = "http://localhost:30900"
      , access_key = "cyclops-dev"
      , secret_key = "cyclops-dev-secret"
      , bucket = "cyclops-build-artifacts"
      }
    , repo_mappings =
      [ { pipeline_image = Some "polar-dev:latest"
        , repo_url = "https://github.com/cmu-sei/Polar.git"
        }
      ]
    }
