-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

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
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/e2334448bd4bb6348a467244d474f907d3d0e36d/dhall/prelude.dhall
        sha256:b81e69ef2fe811bc853a8a9a0202c0af802f7cd53c78f95f67083bf3dceee86b

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
          ,  Lib.nixpkgs "openssl"
          ]
      ]
  , extraEnv =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
