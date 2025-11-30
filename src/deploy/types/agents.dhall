let kubernetes = ./kubernetes.dhall

-- a kubernetes type we copied
let SecretKeySelector =
      { key : Text
      , name : Optional Text
      , optional : Optional Bool
      }

let GraphConfig =
      { graphDB : Text
      , graphUsername : Text
      , graphPassword : SecretKeySelector
      }


let ClientTlsConfig =
      { broker_endpoint : Text
      , server_name : Text
      , client_certificate_path : Text
      , client_key_path : Text
      , client_ca_cert_path : Text
      }


let GraphConfig =
      { graphDB : Text
      , graphUsername : Text
      , graphPassword : SecretKeySelector
      }

let GitlabObserver =
      { name : Text
      , image : Text
      , base_interval_secs : Natural
      , max_backoff_secs : Natural
      , gitlab_endpoint : Text
      , gitlab_token : Optional Text
      , tls : ClientTlsConfig
      }

let GitlabConsumer = { graph : GraphConfig, tls : ClientTlsConfig }

let KubeObserver =
      { name : Text
      , image : Text
      , tls : ClientTlsConfig
      , serviceAccountName : Text
      , secretName : Text
      }

let KubeConsumer =
      { name : Text, image : Text, graph : GraphConfig, tls : ClientTlsConfig }

let ProvenanceLinker = { name : Text, image : Text, tls : ClientTlsConfig, graph : GraphConfig }

let ProvenanceResolver =
      { name : Text, image : Text, tls : ClientTlsConfig }

in  {
    , SecretKeySelector
    , ClientTlsConfig
    , GraphConfig
    , GitlabObserver
    , GitlabConsumer
    , KubeObserver
    , KubeConsumer
    , ProvenanceLinker
    , ProvenanceResolver
    }
