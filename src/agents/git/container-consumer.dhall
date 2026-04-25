-- src/agents/git/container-consumer.dhall

let Lib = ../containers/contaienr-lib.dhall

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "git-processor"
  , entrypoint = Some "git-repo-processor"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
