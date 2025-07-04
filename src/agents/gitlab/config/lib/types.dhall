let ClientTLSConfig =
      { broker_endpoint : Text
      , server_name : Text
      , proxy_ca_cert_path : Optional Text
      , client_certificate_path : Text
      , client_key_path : Text
      , client_ca_cert_path : Text
      }

let GitlabObserver =
      { base_interval_secs : Natural
      , max_backoff_secs : Natural
      , gitlab_endpoint : Text
      , gitlab_token : Optional Text
      , tls : ClientTLSConfig
      }

let GitlabConsumer =
      { graphDB : Text
      , graphUsername : Text
      , graphPassword : Text
      , tls : ClientTLSConfig
      }

in  { ClientTLSConfig, GitlabObserver, GitlabConsumer }
