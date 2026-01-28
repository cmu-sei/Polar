let ImageTag =
      env:POLAR_IMAGE_TAG as Text ? "latest"

let Registry =
      "registry.sandbox.labz.s-box.org/sei/polar-mirror"

let image =
      \(name : Text) ->
        "${Registry}/${name}:${ImageTag}"

let caCert =
      "./certs/ca_certificates/ca_certificate.pem:/etc/ssl/ca_certificate.pem:ro"

let clientKey =
      "./certs/client/client_cassini_key.pem:/etc/ssl/tls.key:ro"

let clientCert =
      "./certs/client/client_cassini_certificate.pem:/etc/ssl/tls.crt:ro"

let proxyCa =
      "./certs/proxy_ca.pem:/etc/ssl/proxy_ca.pem:ro"

let tlsClientVolumes =
      [ caCert, clientKey, clientCert ]

let tlsClientEnv =
      [ "BROKER_ADDR=cassini:8080"
      , "CASSINI_SERVER_NAME=cassini"
      , "TLS_CA_CERT=/etc/ssl/ca_certificate.pem"
      , "TLS_CLIENT_CERT=/etc/ssl/tls.crt"
      , "TLS_CLIENT_KEY=/etc/ssl/tls.key"
      ]

let polarService =
      \(extra :
          { image : Text
          , depends_on : List Text
          , environment : List Text
          , volumes : List Text
          }
      ) ->
        extra //
        { networks = [ "polar" ] }


        in
        { networks.polar = None <>

        , services =
          { cassini =
            polarService
              { image = image "cassini"
              , depends_on = [ "jaeger" ]
              , environment =
                  [ "BROKER_ADDR=localhost:8080"
                  , "TLS_CA_CERT=/etc/ssl/ca_certificate.pem"
                  , "TLS_SERVER_CERT_CHAIN=/etc/ssl/server_cassini_certificate.pem"
                  , "TLS_SERVER_KEY=/etc/ssl/server_cassini_key.pem"
                  , "RUST_LOG=debug"
                  ]
              , volumes =
                  [ caCert
                  , "./certs/server/server_cassini_key.pem:/etc/ssl/server_cassini_key.pem:ro"
                  , "./certs/server/server_cassini_certificate.pem:/etc/ssl/server_cassini_certificate.pem:ro"
                  ]
              } //
              { ports = [ "8080:8080" ] }

          , gitlab-consumer =
            polarService
              { image = image "polar-gitlab-consumer"
              , depends_on = [ "cassini" ]
              , environment =
                  tlsClientEnv
                  # [ "GRAPH_DB=neo4j"
                    , "GRAPH_USER=neo4j"
                    , "GRAPH_PASSWORD=somepassword"
                    , "GRAPH_ENDPOINT=bolt://neo4j:7687"
                    ]
              , volumes = tlsClientVolumes
              }

          , gitlab-observer =
            polarService
              { image = image "polar-gitlab-observer"
              , depends_on = [ "cassini" ]
              , environment =
                  tlsClientEnv
                  # [ "PROXY_CA_CERT=/etc/ssl/proxy_ca.pem"
                    , "OBSERVER_BASE_INTERVAL=30"
                    ]
              , volumes = tlsClientVolumes # [ proxyCa ]
              } //
              { env_file = "gitlab.env" }

          , provenance-linker =
            polarService
              { image = image "polar-linker-agent"
              , depends_on = [ "cassini" ]
              , environment =
                  tlsClientEnv
                  # [ "GRAPH_DB=neo4j"
                    , "GRAPH_USER=neo4j"
                    , "GRAPH_PASSWORD=somepassword"
                    , "GRAPH_ENDPOINT=neo4j://neo4j:7474"
                    ]
              , volumes = tlsClientVolumes
              }

          , provenance-resolver =
            polarService
              { image = image "polar-resolver-agent"
              , depends_on = [ "cassini" ]
              , environment =
                  tlsClientEnv
                  # [ "PROXY_CA_CERT=/etc/ssl/proxy_ca.pem"
                    , "DOCKER_CONFIG=/home/polar/.docker/"
                    ]
              , volumes =
                  tlsClientVolumes
                  # [ proxyCa
                    , "./config.json:/home/polar/.docker/config.json"
                    ]
              }

          , jaeger =
            { image = "cr.jaegertracing.io/jaegertracing/jaeger:2.11.0"
            , environment = [ "COLLECTOR_OTLP_ENABLED=true" ]
            , networks = [ "polar" ]
            , ports =
                [ "16686:16686"
                , "4317:4317"
                , "4318:4318"
                , "5778:5778"
                , "9411:9411"
                ]
            }

          , neo4j =
            { image = "registry1.dso.mil/ironbank/opensource/neo4j/neo4j:5.26.2"
            , environment = [ "NEO4J_AUTH=neo4j/somepassword" ]
            , networks = [ "polar" ]
            , ports = [ "7474:7474", "7687:7687" ]
            , restart = "never"
            , user = "7474:7474"
            , volumes = [ "./neo4j_setup/imports:/var/lib/neo4j/import" ]
            }
          }

        , volumes.neo4j-data = None <>
        }
