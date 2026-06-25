-- src/agents/polar-init/container.dhall

let Lib = ../../containers/container-lib.dhall

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "polar-init"
  , entrypoint = Some "polar-init"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , packageLayers =
      defaults.minimalContainer.packageLayers #
      [ Lib.customLayer "polar-init-deps"
          [ Lib.nixpkgs "cacert"
          , Lib.nixpkgs "openssl"
          ]
      ]
  , extraEnv =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
