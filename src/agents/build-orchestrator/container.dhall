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
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/e2334448bd4bb6348a467244d474f907d3d0e36d/dhall/prelude.dhall
        sha256:b81e69ef2fe811bc853a8a9a0202c0af802f7cd53c78f95f67083bf3dceee86b

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
