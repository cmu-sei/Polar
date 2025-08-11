let GitlabAgent = ./types/gitlab-agent.dhall
let ClientTLSConfig = ./types/cassini-client.dhall

let tls
    : ClientTLSConfig
    = { broker_endpoint = env:BROKER_ADDR as Text
      , server_name = env:CASSINI_SERVER_NAME as Text
      , proxy_ca_cert_path = Some (env:PROXY_CA_CERT as Text ? "")
      , client_certificate_path = env:TLS_CLIENT_CERT as Text
      , client_key_path = env:TLS_CLIENT_KEY as Text
      , client_ca_cert_path = env:TLS_CA_CERT as Text
      }

let gitlabObserver
    : GitlabAgent.GitlabObserver
    = { base_interval_secs = 300
      , max_backoff_secs = 600
      , gitlab_endpoint = env:GITLAB_ENDPOINT as Text
      , gitlab_token = Some env:GITLAB_TOKEN as Text
      , tls
      }

in  gitlabObserver
