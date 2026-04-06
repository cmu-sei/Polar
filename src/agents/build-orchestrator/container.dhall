-- container.dhall
-- Minimal init container for Cyclops git-clone jobs.
--
-- This container runs as a Kubernetes init container in Cyclops build jobs.
-- Its sole responsibility is to clone a git repository at a specific commit
-- SHA into /workspace, then exit 0. The pipeline container starts only after
-- this succeeds.
--
-- The entrypoint binary (git-clone-entrypoint) is built locally via
-- writeShellApplication in package.nix and passed to mkContainer via
-- extraDerivations — it cannot be expressed as a dhall PackageRef because
-- it is not a flake input or a nixpkgs package.
--
-- Runtime dependencies (git, cacert) are declared here in packageLayers
-- so they are included in the image and visible to the entrypoint script.
--
-- Security notes:
--   - Runs as UID 65532 (conventional non-root for k8s init containers).
--   - Credentials are injected as env vars by the orchestrator at runtime,
--     never baked into the image.
--   - The askpass script writes to /tmp (tmpfs) and is removed after clone.
--   - .git directory is stripped from /workspace after checkout.
let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/bc1246f3372fbb825de2a85e6f3ca9d0779975d5/dhall/prelude.dhall
        sha256:42b061b5cb6c7685afaf7e5bc6210640d2c245e67400b22c51e6bfdf85a89e06

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "cyclops/git-clone"
  , entrypoint = Some "git-clone-entrypoint"
  , staticUid  = Some 65532
  , staticGid  = Some 65532
  , packageLayers =
      [ Lib.PackageLayer.Core
      , Lib.customLayer "git-clone-deps"
          [ Lib.nixpkgs "git"
          , Lib.nixpkgs "cacert"
          ]
      ]
  , extraEnv =
      [ Lib.buildEnv "GIT_SSL_CAINFO" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "GIT_TERMINAL_PROMPT" "0"
      ]
  }
