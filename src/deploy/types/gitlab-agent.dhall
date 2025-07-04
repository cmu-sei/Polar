let ClientTLSConfig = ./cassini-client.dhall

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

in  {  GitlabObserver, GitlabConsumer }
