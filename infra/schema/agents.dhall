let kubernetes =
      ./kubernetes.dhall
        sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

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

let PolarAgent = { name : Text, image : Text, tls : ClientTlsConfig }

let GraphProcessor = PolarAgent //\\ { graph : GraphConfig }

let WithServiceAccount = { serviceAccountName : Text, secretName : Text }

let GitConsumer = GraphProcessor

let GitScheduler = GraphProcessor

let ProvenanceLinker = GraphProcessor

let KubeConsumer = GraphProcessor

let GitlabConsumer = GraphProcessor

let BuildProcessor = GraphProcessor

let GitObserver = PolarAgent

let BuildOrchestrator = PolarAgent //\\ WithServiceAccount

let KubeObserver = PolarAgent //\\ WithServiceAccount

let ProvenanceResolver = PolarAgent

let GitlabObserver =
          PolarAgent
      //\\  { baseIntervalSecs : Natural
            , maxBackoffSecs : Natural
            , endpoint : Text
            , token : Optional Text
            }

in  { ClientTlsConfig
    , GraphConfig
    , PolarAgent
    , GraphProcessor
    , WithServiceAccount
    , GitlabObserver
    , GitlabConsumer
    , KubeObserver
    , KubeConsumer
    , ProvenanceLinker
    , ProvenanceResolver
    , GitObserver
    , GitConsumer
    , GitScheduler
    , BuildOrchestrator
    , BuildProcessor
    }
