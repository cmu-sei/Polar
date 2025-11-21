{ apiVersion = "v1"
, kind = "Service"
, metadata = { name = "polar-db-svc", namespace = "polar-db" }
, spec =
  { ports =
    [ { appProtocol = None Text
      , name = "http-ui"
      , port = 7473
      , protocol = "TCP"
      , targetPort = 7473
      }
    , { appProtocol = Some "kubernetes.io/wss"
      , name = "bolt"
      , port = 7687
      , protocol = "TCP"
      , targetPort = 7687
      }
    ]
  , selector.name = "polar-neo4j"
  , type = "ClusterIP"
  }
}
