let orchestrator = ./orchestrator.dhall

in  orchestrator.OrchestratorConfig::{
    , backend =
      { driver = "kubernetes"
      , command = ["cargo", "build"]
      , job_env =
        [ { name = "BROKER_ADDR", value = "cassini-ip-svc.polar.svc.cluster.local:8080" }
        , { name = "CASSINI_SERVER_NAME", value = "cassini-ip-svc.polar.svc.cluster.local" }
        ]
      , kubernetes = Some
        { job_labels = [ { mapKey = "cyclops.io/env", mapValue = "dev" } ]
        , namespace = "default"
        , ca_bundle_secret = None Text
        , workspace_size_limit = Some "4Gi"
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
      -- Dev object store (deploy/dev.dhall), by its in-cluster name.
      , endpoint_url = "http://object-store.cyclops-dev.svc.cluster.local:9000"
      , access_key = "cyclops-dev"
      , secret_key = "cyclops-dev-secret"
      , bucket = "cyclops-build-artifacts"
      }
    , repo_mappings =
      [ { pipeline_image = Some "rust:latest"
        , repo_url = "https://github.com/cmu-sei/Polar.git"
        }
      ]
    }
