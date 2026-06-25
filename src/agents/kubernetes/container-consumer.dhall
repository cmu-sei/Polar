-- src/agents/kubernetes/container-consumer.dhall

let Lib = ../../containers/container-lib.dhall

let defaults = Lib.defaults

let healthcheckLayer =
  Lib.customLayer "polar-healthcheck-bin"
    [ Lib.flakePackage "polar-healthcheck" "default"
    ]

let polarInitLayer =
  Lib.customLayer "polar-init-bin"
    [ Lib.flakePackage "polar-init" "default"
    ]

in defaults.minimalContainer //
  { name       = "kube-consumer"
  , entrypoint = Some "kube-consumer"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , packageLayers = defaults.minimalContainer.packageLayers # [ healthcheckLayer, polarInitLayer ]
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE"       "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"        "/etc/ssl/certs"
      , Lib.buildEnv "POLAR_HEALTH_CERTS"  "/etc/tls/certs/cert.pem,/etc/neo4j-client-tls/cert.pem"
      ]
  }
