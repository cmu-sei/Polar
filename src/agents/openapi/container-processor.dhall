-- src/agents/openapi/container-processor.dhall

let Lib = ../containers/container-lib.dhall

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "openapi-processor"
  , entrypoint = Some "openapi-processor"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
