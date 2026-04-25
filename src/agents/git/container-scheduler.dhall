-- src/agents/git/container-scheduler.dhall

let Lib = ../containers/contaienr-lib.dhall

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "git-scheduler"
  , entrypoint = Some "scheduler"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
