-- TODO: Figure out a good configuration schema for the gitlab observer
let GitlabConfig = {
  graphql_endpoint : Text,
  token : Text,
  proxy_ca_cert_file : Optional Text,
  tls : {
    client_cert : Text,
    client_key : Text,
    ca_cert : Text
  },
  broker : {
    address : Text,
    server_name : Text
  },
  base_interval_secs : Natural,
  initial_backoff_secs : Natural,
  max_backoff_secs : Natural
}

let gitlabConfig : GitlabConfig = {
    base_interval_secs = 300,
    initial_backoff_secs = 60,
    max_backoff_secs = 1800,
    graphql_endpoint = env:GITLAB_ENDPOINT as Text,
    token = env:GITLAB_TOKEN as Text,
    proxy_ca_cert_file = Some env:PROXY_CA_CERT as Text,
    
    tls = {
        client_cert = env:TLS_CLIENT_CERT as Text,
        client_key = env:TLS_CLIENT_KEY as Text,
        ca_cert = env:TLS_CA_CERT as Text
    },
    broker = {
        address = env:BROKER_ADDR as Text,
        server_name = env:CASSINI_SERVER_NAME as Text
    },

}

in gitlabConfig
