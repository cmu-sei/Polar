-- src/agents/build-orchestrator/container-orchestrator.dhall

let Lib = ../../containers/container-lib.dhall
let defaults = Lib.defaults

let healthcheckLayer =
  Lib.customLayer "polar-healthcheck-bin"
    [ Lib.flakePackage "polar-healthcheck" "default"
    ]

in defaults.minimalContainer //
  { name       = "build-orchestrator"
  , entrypoint = Some "build-orchestrator"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , packageLayers = defaults.minimalContainer.packageLayers # [ healthcheckLayer ]
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE"      "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"       "/etc/ssl/certs"
      , Lib.buildEnv "POLAR_HEALTH_CERTS" "/etc/tls/certs/cert.pem"
      ]
  }
