let orchestrator = ./orchestrator.dhall

in  orchestrator.OrchestratorConfig::{
    , backend =
      { driver = "kubernetes"
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
      }
    , bootstrap =
      { builder_image = "docker.io/nixos/nix:2.24.1"
      }
    , storage = orchestrator.StorageConfig::{
      , endpoint_url = "http://localhost:9000"
      , access_key = "minio"
      , secret_key = "minio123"
      , bucket = "cyclops-build-artifacts"
      }
    , cassini =
      { broker_url = "cassini://localhost:7400"
      , inbound_subject = "cyclops.build.requested"
      }
    , credentials =
      { git_secret_name = "cyclops-git-credentials"
      , registry_secret_name = "cyclops-registry-credentials"
      }
    , log = { format = "pretty", level = "cyclops=debug,warn" }
    , repo_mappings =
      [ { pipeline_image = Some "rust:latest"
        , repo_url = "https://github.com/cmu-sei/Polar.git"
        }
      ]
    }
