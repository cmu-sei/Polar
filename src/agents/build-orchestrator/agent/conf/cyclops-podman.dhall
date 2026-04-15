let orchestrator = ./orchestrator.dhall

in  orchestrator.OrchestratorConfig::{
    , backend =
    { driver = "podman"
    , command = ["cargo", "version" ]
    , kubernetes = None orchestrator.KubernetesBackendConfig.Type
    , podman = Some orchestrator.PodmanBackendConfig::{ socket_path = env:PODMAN_DEV_SOCKET as Text }
    }
    , storage = orchestrator.StorageConfig::{
      , endpoint_url = "http://localhost:9000"
      , access_key = "minio"
      , secret_key = "minio123"
      , bucket = "cyclops-build-artifacts"
      }
    , repo_mappings =
      [ { pipeline_image = Some "polar-dev:latest"
        , repo_url = "https://github.com/cmu-sei/Polar.git"
        }
      ]
    }
