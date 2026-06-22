let orchestrator = ./orchestrator.dhall

in  orchestrator.OrchestratorConfig::{
    , backend =
      { driver = "kubernetes"
      , command = ["cargo", "build"]
      , kubernetes = Some
        { job_labels = [ { mapKey = "cyclops.io/env", mapValue = "dev" } ]
        , namespace = "default"
        , resources = Some
          { cpu_limit = Some "2"
          , cpu_request = Some "500m"
          , memory_limit = Some "4Gi"
          , memory_request = Some "512Mi"
          }
        }
      , podman = None orchestrator.PodmanBackendConfig.Type
      }
    , storage = orchestrator.StorageConfig::{
      , endpoint_url = "http://localhost:9000"
      , access_key = "minio"
      , secret_key = "minio123"
      , bucket = "cyclops-build-artifacts"
      }
    , repo_mappings =
      [ { pipeline_image = Some "rust:latest"
        , repo_url = "https://github.com/cmu-sei/Polar.git"
        }
      ]
    }
