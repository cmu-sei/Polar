let kubernetes = ./kubernetes.dhall


let GraphConfig =
      { graphDB : Text
      , graphUsername : Text
      , graphPassword : kubernetes.SecretKeySelector.Type
      }

let ClientTlsConfig =
      { broker_endpoint : Text
      , server_name : Text
      , client_certificate_path : Text
      , client_key_path : Text
      , client_ca_cert_path : Text
      }

-- Base type. Every agent in the system has at minimum these three fields.
let PolarAgent =
      { name : Text
      , image : Text
      , tls : ClientTlsConfig
      }

-- Structural extensions. Use these to build concrete agent types.
let GraphProcessor = PolarAgent //\\ { graph : GraphConfig }

let WithConfig = PolarAgent //\\ { config : Text }

let WithServiceAccount =
      { serviceAccountName : Text, secretName : Text }

-- Concrete agent type aliases. These exist purely for documentation clarity
-- at the call site; structurally they are identical to their base.
let GitConsumer = GraphProcessor
let GitScheduler = GraphProcessor
let ProvenanceLinker = GraphProcessor
let KubeConsumer = GraphProcessor
let GitlabConsumer = GraphProcessor

let GitObserver = WithConfig
let BuildOrchestrator = WithConfig //\\ WithServiceAccount

let BuildProcessor = GraphProcessor

let KubeObserver = PolarAgent //\\ WithServiceAccount

-- ProvenanceResolver doesn't need graph access, just the base agent + TLS.
-- Keeping it as a named alias makes intent explicit at the values.dhall level.
let ProvenanceResolver = PolarAgent

-- GitlabObserver is the only genuine structural outlier.
let GitlabObserver =
      PolarAgent
        //\\ { baseIntervalSecs : Natural
             , maxBackoffSecs : Natural
             , endpoint : Text
             , token : Optional Text
             }

{- ============================================================================
   Git Agent Static Credential Configuration Types
   ----------------------------------------------------------------------------
-}
let HttpCredential = { username : Text, token : Text }

let HostCredentialConfig = { http : Optional HttpCredential }

let StaticCredentialConfig =
      { hosts : List { mapKey : Text, mapValue : HostCredentialConfig } }

in  {
    ClientTlsConfig
    , GraphConfig
    , PolarAgent
    , GraphProcessor
    , WithConfig
    , WithServiceAccount
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
    , BuildOrchestrator
    , BuildProcessor
    }
