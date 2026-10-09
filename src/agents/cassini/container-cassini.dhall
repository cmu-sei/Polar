-- src/agents/cassini/container-cassini.dhall

let Lib = ../../containers/container-lib.dhall

let defaults = Lib.defaults

-- The liveness probe the catalog gives every workload runs this.
let healthcheckLayer =
  Lib.customLayer "polar-healthcheck-bin"
    [ Lib.flakePackage "polar-healthcheck" "default"
    ]

in defaults.minimalContainer //
  { name       = "cassini"
  , entrypoint = Some "cassini-server"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , packageLayers = defaults.minimalContainer.packageLayers # [ healthcheckLayer ]
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
