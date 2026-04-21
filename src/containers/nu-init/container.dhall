-- src/containers/nu-init/container.dhall
--
-- Minimal nushell init container for Polar infrastructure.
--
-- Contract: mount a nushell script at /scripts/init.nu and this container
-- will execute it. The container has no opinion about what the script does.
-- Infrastructure automation (infra/layers/) owns the scripts.
--
-- Usage in a pod spec:
--   initContainers:
--     - name: nu-init
--       image: polar-nu-init:latest
--       volumeMounts:
--         - name: init-script
--           mountPath: /scripts
--   volumes:
--     - name: init-script
--       configMap:
--         name: <your-configmap>
--         items:
--           - key: init.nu
--             path: init.nu
--
-- To regenerate container.nix after editing this file:
--   cd src/containers/nu-init && just render

let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/9da4924831c8e0d81d57448425d6cd10820b71d2/dhall/prelude.dhall
        sha256:18acbbb5708565905ab9522fa77a81eb402851f06870a34a22f6c979001c4571

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "polar-nu-init"
  , entrypoint = Some "nu-init-entrypoint"
  , staticUid  = Some 65532
  , staticGid  = Some 65532
  , packageLayers =
      [ Lib.PackageLayer.Core
      , Lib.customLayer "nu-init-deps"
          [ Lib.nixpkgs "cacert"
          ]
      ]
  , extraEnv =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
