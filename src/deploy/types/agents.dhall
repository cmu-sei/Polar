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

let CertClientConfig =
      { sa_token_path : Text
      , cert_issuer_url : Text
      , audience : Text
      , cert_dir : Text
      , cert_type : Text
      }

let PolarAgent =
      { name : Text
      , image : Text
      , certClient : CertClientConfig
      , tls : ClientTlsConfig
      }

let GraphProcessor = PolarAgent //\\ { graph : GraphConfig }

let WithConfig = PolarAgent //\\ { config : Text }

let WithServiceAccount = { serviceAccountName : Text, secretName : Text }

let CertIssuer =
      { name : Text, image : Text, config : Text, serviceAccountName : Text }

let GitConsumer = GraphProcessor

let GitScheduler = GraphProcessor

let ProvenanceLinker = GraphProcessor

let KubeConsumer = GraphProcessor

let GitlabConsumer = GraphProcessor

let GitObserver = WithConfig

let BuildOrchestrator = WithConfig //\\ WithServiceAccount

let BuildProcessor = GraphProcessor

let KubeObserver = PolarAgent //\\ WithServiceAccount

let ProvenanceResolver = PolarAgent

let GitlabObserver =
          PolarAgent
      //\\  { baseIntervalSecs : Natural
            , maxBackoffSecs : Natural
            , endpoint : Text
            , token : Optional Text
            }

let HttpCredential = { username : Text, token : Text }

let HostCredentialConfig = { http : Optional HttpCredential }

let StaticCredentialConfig =
      { hosts : List { mapKey : Text, mapValue : HostCredentialConfig } }

in  { ClientTlsConfig
    , GraphConfig
    , CertClientConfig
    , PolarAgent
    , GraphProcessor
    , WithConfig
    , WithServiceAccount
    , CertIssuer
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
