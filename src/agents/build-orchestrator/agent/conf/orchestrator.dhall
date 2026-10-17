-- Typed schema for Cyclops orchestrator configuration.
-- Compile to YAML with: dhall-to-yaml --file config/dev.dhall > cyclops.yaml
let Map = List { mapKey : Text, mapValue : Text }

let ResourceConfig =
      { Type =
          { cpu_limit : Optional Text
          , memory_limit : Optional Text
          , cpu_request : Optional Text
          , memory_request : Optional Text
          }
      , default =
        { cpu_limit = None Text
        , memory_limit = None Text
        , cpu_request = None Text
        , memory_request = None Text
        }
      }

let KubernetesBackendConfig =
      { Type =
          { namespace : Text
          , job_labels : Map
          , resources : Optional ResourceConfig.Type
            -- Size limit of each job's workspace emptyDir, e.g. "2Gi".
          , workspace_size_limit : Optional Text
            -- Secret whose `ca-bundle.crt` replaces job containers' trust
            -- store (TLS-inspecting proxies). Must include public roots.
          , ca_bundle_secret : Optional Text
          }
      , default =
        { job_labels = [] : Map
        , resources = None ResourceConfig.Type
        , workspace_size_limit = None Text
        , ca_bundle_secret = None Text
        }
      }

let PodmanBackendConfig =  { Type = { socket_path : Text, image_pull_policy: Optional Text } , default = { socket_path = "/var/run/docker.sock", image_pull_policy = Some "IfNotPresent" } }

let BackendConfig =
      { Type =
          { driver : Text
          , command : List Text
            -- Env injected into every job container (BROKER_ADDR, CASSINI_SERVER_NAME, ...)
          , job_env : List { name : Text, value : Text }
          , kubernetes : Optional KubernetesBackendConfig.Type
          , podman : Optional PodmanBackendConfig.Type
          }
      }

let RepoMapping =
      { Type = { repo_url : Text, pipeline_image : Optional Text }
      , default.pipeline_image = None Text
      }


let StorageConfig =
      { Type =
          { endpoint_url : Text
          , access_key : Text
          , secret_key : Text
          , region : Text
          , bucket : Text
          }
      , default.region = "us-east-1"
      }

let OrchestratorConfig =
      { Type =
          { backend : BackendConfig.Type
          , repo_mappings : List RepoMapping.Type
          , storage : StorageConfig.Type
          }
      , default =
        { repo_mappings = [] : List RepoMapping.Type }
      }

in  { OrchestratorConfig
    , BackendConfig
    , KubernetesBackendConfig
    , PodmanBackendConfig
    , ResourceConfig
    , StorageConfig
    , RepoMapping
    }
