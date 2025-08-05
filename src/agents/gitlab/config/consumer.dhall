let types = ./lib/types.dhall

let tls :
      types.ClientTLSConfig = {
        broker_addr = env:BROKER_ADDR as Text,
        server_name = env:CASSINI_SERVER_NAME as Text,
        proxy_ca_cert_path = Some (env:PROXY_CA_CERT as Text ? ""),
        client_certificate_path = env:TLS_CLIENT_CERT as Text,
        client_key_path = env:TLS_CLIENT_KEY as Text,
        client_ca_cert_path = env:TLS_CA_CERT as Text
      }

let gitlabConsumer :
      types.GitlabConsumer = {
        graphDB = env:GRAPH_DB as Text,
        graphUsername = env:GRAPH_USER as Text,
        graphPassword = env:GRAPH_PASSWORD as Text,
        tls = tls
      }

in  gitlabConsumer
