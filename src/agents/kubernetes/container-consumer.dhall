-- src/agents/kubernetes/container-consumer.dhall
let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/9b998a47b8042a932c1f4b0dc0e6ce957d26757d/dhall/prelude.dhall
        sha256:f75818ad203cb90a5e5921b75cd60bcb66ac5753cf7eba976538bf71e855378c

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "kube-consumer"
  , entrypoint = Some "kube-consumer"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
