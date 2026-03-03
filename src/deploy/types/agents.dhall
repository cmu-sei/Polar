let kubernetes = ./kubernetes.dhall

let SecretKeySelector =
      { key : Text, name : Optional Text, optional : Optional Bool }

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

{- ============================================================================
    Git Agent Static Credential Configuration Types
    ----------------------------------------------------------------------------
-}

let HttpCredential =
    { username : Text
    , token : Text
    }

let HostCredentialConfig =
    { http : Optional HttpCredential
    }

let StaticCredentialConfig =
    { hosts : List
        { mapKey : Text
        , mapValue : HostCredentialConfig
        }
    }

let GitlabObserver =
      { name : Text
      , image : Text
      , baseIntervalSecs : Natural
      , maxBackoffSecs : Natural
      , endpoint : Text
      , token : Optional Text
      , tls : ClientTlsConfig
      }

let GitlabConsumer = { name: Text, image: Text, graph : GraphConfig, tls : ClientTlsConfig }

let KubeObserver =
      { name : Text
      , image : Text
      , tls : ClientTlsConfig
      , serviceAccountName : Text
      , secretName : Text
      }

let KubeConsumer =
      { name : Text, image : Text, graph : GraphConfig, tls : ClientTlsConfig }

let ProvenanceLinker =
      { name : Text, image : Text, tls : ClientTlsConfig, graph : GraphConfig }



-- simple data types to configure the resolver with.
let Registry =
    { name : Text
    , url : Text
    , clientCertPath : Optional Text
    }

let ResolverConfig =
    { registries : List Registry }

-- TODO: Add resolver config as a field
let ProvenanceResolver = { name : Text, image : Text, tls : ClientTlsConfig }

let GitObserver = { name : Text, image : Text, tls: ClientTlsConfig, config: Text }

let GitConsumer = { name : Text, image : Text, tls: ClientTlsConfig, graph : GraphConfig }

let GitScheduler = { name : Text, image : Text, tls: ClientTlsConfig, graph : GraphConfig }

in  { SecretKeySelector
    , ClientTlsConfig
    , GraphConfig
    , GitlabObserver
    , GitlabConsumer
    , KubeObserver
    , KubeConsumer
    , ProvenanceLinker
    , ProvenanceResolver
    , StaticCredentialConfig
    , GitObserver
    , GitConsumer
    , GitScheduler
    }
