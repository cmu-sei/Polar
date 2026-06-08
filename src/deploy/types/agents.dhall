let kubernetes = ./kubernetes.dhall
let constants = ./lib-constants.dhall
let PolarNamespace = constants.polarNamespace

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
      { Type =
          { sa_token_path : Text
          , cert_issuer_url : Text
          , audience : Text
          , cert_dir : Text
          , cert_type : Text
          , key_algorithm : Text
          , extra_sans : Optional Text
          }
      , default =
        { sa_token_path = "/home/polar/sa-token/token"
        , cert_issuer_url =
            "http://cert-issuer.${PolarNamespace}.svc.cluster.local:8443"
        , audience = "polar-cert-issuer.local"
        , cert_dir = constants.certDir
        , cert_type = "client"
        , key_algorithm = "ecdsa-p256"
        , extra_sans = None Text
        }
      }

let PolarAgent =
      { name : Text
      , image : Text
      , certClient : CertClientConfig.Type
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

let OciResolver = PolarAgent

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
    , OciResolver
    , StaticCredentialConfig
    , GitObserver
    , GitConsumer
    , GitScheduler
    , BuildProcessor
    }
