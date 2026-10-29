-- src/agents/polar-operator/container.dhall
--
-- The operator's image: the binary and the CA bundle it needs to reach the API server. Pre-rendered
-- to container.nix with `dhall-to-nix < container.dhall > container.nix` (see `just` recipe
-- `update-container-lib`), because Nix cannot fetch the library inside its sandbox.

let Lib = ../../containers/container-lib.dhall

let defaults = Lib.defaults

in  defaults.minimalContainer //
      { name       = "polar-operator"
      , entrypoint = Some "polar-operator"
      , staticUid  = Some 65532
      , staticGid  = Some 65532
      , extraEnv =
          [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
          , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
          ]
      }
