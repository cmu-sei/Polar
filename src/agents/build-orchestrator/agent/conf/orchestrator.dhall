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
          }
      , default =
        { job_labels = [] : Map, resources = None ResourceConfig.Type }
      }

let PodmanBackendConfig =  { Type = { socket_path : Text, image_pull_policy: Optional Text } , default = { socket_path = "/var/run/docker.sock", image_pull_policy = Some "IfNotPresent" } }

let BackendConfig =
      { Type =
          { driver : Text, command : List Text, kubernetes : Optional KubernetesBackendConfig.Type, podman : Optional PodmanBackendConfig.Type }
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
