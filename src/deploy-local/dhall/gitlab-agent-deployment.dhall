{ apiVersion = "apps/v1"
, kind = "Deployment"
, metadata =
  { annotations.`sidecar.istio.io/inject` = "false"
  , name = "gitlab-agent"
  , namespace = "polar"
  }
, spec =
  { replicas = 1
  , selector.matchLabels.name = "gitlab-agent"
  , template =
    { metadata = { labels.name = "gitlab-agent", name = "gitlab-agent" }
    , spec =
      { containers =
        [ { env =
            [ { name = "SSL_CERT_FILE"
              , value = Some "/etc/site/gitlab.crt"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "PROXY_CA_CERT"
              , value = Some "/etc/proxy/proxy.pem"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "TLS_CA_CERT"
              , value = Some "/etc/tls/ca.crt"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "TLS_CLIENT_CERT"
              , value = Some "/etc/tls/tls.crt"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "TLS_CLIENT_KEY"
              , value = Some "/etc/tls/tls.key"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "BROKER_ADDR"
              , value = Some "cassini-ip-svc.polar.svc.cluster.local:8080"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "CASSINI_SERVER_NAME"
              , value = Some "cassini-ip-svc.polar.svc.cluster.local"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "OBSERVER_BASE_INTERVAL"
              , value = Some "300"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "GITLAB_ENDPOINT"
              , value = Some "https://YOUR_GITLAB_HOST"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "GITLAB_TOKEN"
              , value = None Text
              , valueFrom = Some
                { secretKeyRef = { key = "token", name = "gitlab-secret" } }
              }
            ]
          , image = "polar-gitlab-observer:latest"
          , imagePullPolicy = "Always"
          , name = "polar-gitlab-observer"
          , securityContext =
            { capabilities.drop = [ "ALL" ]
            , runAsGroup = 1000
            , runAsNonRoot = True
            , runAsUser = 1000
            }
          , volumeMounts =
            [ { mountPath = "/etc/tls"
              , name = "client-tls"
              , readOnly = None Bool
              }
            , { mountPath = "/etc/proxy"
              , name = "proxy-ca-cert"
              , readOnly = Some True
              }
            , { mountPath = "/etc/site"
              , name = "gitlab-crt"
              , readOnly = Some True
              }
            ]
          }
        , { env =
            [ { name = "TLS_CA_CERT"
              , value = Some "/etc/tls/ca.crt"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "TLS_CLIENT_CERT"
              , value = Some "/etc/tls/tls.crt"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "TLS_CLIENT_KEY"
              , value = Some "/etc/tls/tls.key"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "BROKER_ADDR"
              , value = Some "cassini-ip-svc.polar.svc.cluster.local:8080"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "CASSINI_SERVER_NAME"
              , value = Some "cassini-ip-svc.polar.svc.cluster.local"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "GRAPH_ENDPOINT"
              , value = Some
                  "neo4j://polar-db-svc.polar-db.svc.cluster.local:7687"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "GRAPH_DB"
              , value = Some "neo4j"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "GRAPH_USER"
              , value = Some "neo4j"
              , valueFrom = None { secretKeyRef : { key : Text, name : Text } }
              }
            , { name = "GRAPH_PASSWORD"
              , value = None Text
              , valueFrom = Some
                { secretKeyRef = { key = "secret", name = "polar-graph-pw" } }
              }
            ]
          , image = "polar-gitlab-consumer:latest"
          , imagePullPolicy = "Always"
          , name = "polar-gitlab-consumer"
          , securityContext =
            { capabilities.drop = [ "ALL" ]
            , runAsGroup = 1000
            , runAsNonRoot = True
            , runAsUser = 1000
            }
          , volumeMounts =
            [ { mountPath = "/etc/tls"
              , name = "client-tls"
              , readOnly = Some True
              }
            ]
          }
        ]
      , volumes =
        [ { name = "client-tls", secret.secretName = "client-tls" }
        , { name = "proxy-ca-cert", secret.secretName = "proxy-ca-cert" }
        , { name = "gitlab-crt", secret.secretName = "gitlab-crt" }
        ]
      }
    }
  }
}
