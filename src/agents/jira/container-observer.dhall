-- src/agents/jira/container-observer.dhall
--
-- Minimal OCI container for the jira-observer binary.

let Lib = ../../containers/container-lib.dhall
let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "jira-observer"
  , entrypoint = Some "jira-observer"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
