-- config/schema.dhall
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

let BackendConfig =
      { Type =
          { driver : Text, kubernetes : Optional KubernetesBackendConfig.Type }
      }

let BootstrapConfig = { Type = { builder_image : Text } }

let CredentialsConfig =
      { Type = { git_secret_name : Text, registry_secret_name : Text } }

let RepoMapping =
      { Type = { repo_url : Text, pipeline_image : Optional Text }
      , default.pipeline_image = None Text
      }

let CassiniConfig = { Type = { broker_url : Text, inbound_subject : Text } }

let LogConfig =
      { Type = { format : Text, level : Text }
      , default = { format = "json", level = "info" }
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
          , cassini : CassiniConfig.Type
          , bootstrap : BootstrapConfig.Type
          , credentials : CredentialsConfig.Type
          , repo_mappings : List RepoMapping.Type
          , log : LogConfig.Type
          , storage : StorageConfig.Type
          }
      , default =
        { repo_mappings = [] : List RepoMapping.Type, log = LogConfig.default }
      }

in  { OrchestratorConfig
    , BackendConfig
    , KubernetesBackendConfig
    , ResourceConfig
    , BootstrapConfig
    , CredentialsConfig
    , StorageConfig
    , RepoMapping
    , CassiniConfig
    , LogConfig
    }
