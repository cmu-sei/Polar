{ apiVersion = "apps/v1"
, kind = "Deployment"
, metadata = { name = "cassini", namespace = "polar" }
, spec =
  { replicas = 1
  , selector.matchLabels.name = "cassini"
  , template =
    { metadata = { labels.name = "cassini", name = "cassini" }
    , spec =
      { containers =
        [ { env =
            [ { name = "TLS_CA_CERT", value = "/etc/tls/ca.crt" }
            , { name = "TLS_SERVER_CERT_CHAIN", value = "/etc/tls/tls.crt" }
            , { name = "TLS_SERVER_KEY", value = "/etc/tls/tls.key" }
            , { name = "CASSINI_BIND_ADDR", value = "0.0.0.0:8080" }
            ]
          , image = "cassini:latest"
          , imagePullPolicy = "IfNotPresent"
          , name = "cassini"
          , ports = [ { containerPort = 8080 } ]
          , securityContext =
            { capabilities.drop = [ "ALL" ]
            , runAsGroup = 1000
            , runAsNonRoot = True
            , runAsUser = 1000
            }
          , volumeMounts =
            [ { mountPath = "/etc/tls", name = "cassini-tls", readOnly = True }
            ]
          }
        ]
      , volumes =
        [ { name = "cassini-tls", secret.secretName = "cassini-tls" } ]
      }
    }
  }
}
