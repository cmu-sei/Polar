{ apiVersion = "apps/v1"
, kind = "Deployment"
, metadata =
  { annotations.`sidecar.istio.io/inject` = "false"
  , name = "kubernetes-agent"
  , namespace = "polar"
  }
, spec =
  { replicas = 1
  , selector.matchLabels.name = "kubernetes-agent"
  , template =
    { metadata = { labels.name = "kubernetes-agent", name = "kubernetes-agent" }
    , spec =
      { containers =
        [ { env =
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
            , { name = "KUBE_TOKEN"
              , value = None Text
              , valueFrom = Some
                { secretKeyRef =
                  { key = "token", name = "kube-observer-sa-token" }
                }
              }
            ]
          , image = "polar-kube-observer:latest"
          , imagePullPolicy = "IfNotPresent"
          , name = "kube-observer"
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
            , { mountPath = "/var/run/secrets/kubernetes.io/serviceaccount"
              , name = "kube-observer-sa-token"
              , readOnly = None Bool
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
          , image = "polar-kube-consumer:latest"
          , imagePullPolicy = "IfNotPresent"
          , name = "kube-consumer"
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
      , serviceAccountName = "kube-observer-sa"
      , volumes =
        [ { name = "client-tls", secret.secretName = "client-tls" }
        , { name = "kube-observer-sa-token"
          , secret.secretName = "kube-observer-sa-token"
          }
        ]
      }
    }
  }
}
