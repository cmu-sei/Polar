-- src/containers/git-server/container.dhall
--
-- Minimal git HTTP server container for local development.
--
-- Serves local git repositories over HTTP using nginx + fcgiwrap +
-- git-http-backend. Intended to be run on the host via podman, NOT
-- deployed into the cluster.
--
-- Repositories are mounted into the container at runtime via podman -v flags.
-- The nginx config reads from /etc/git-server/repos.conf to determine which
-- paths to serve and under which names.
--
-- Usage:
--   See src/containers/git-server/Justfile and README.md
--
-- To regenerate container.nix after editing this file:
--   cd src/containers/git-server && just render

let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/bc1246f3372fbb825de2a85e6f3ca9d0779975d5/dhall/prelude.dhall
        sha256:42b061b5cb6c7685afaf7e5bc6210640d2c245e67400b22c51e6bfdf85a89e06

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "polar-git-server"
  , entrypoint = Some "git-server-entrypoint"
  , staticUid  = Some 65532
  , staticGid  = Some 65532
  , packageLayers =
      [ Lib.PackageLayer.Core
      , Lib.customLayer "git-server-deps"
          [ Lib.nixpkgs "cacert"
          , Lib.nixpkgs "git"
          , Lib.nixpkgs "nginx"
          , Lib.nixpkgs "fcgiwrap"
          ]
      ]
  , extraEnv =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      , Lib.buildEnv "GIT_HTTP_EXPORT_ALL" "1"
      , Lib.buildEnv "GIT_PROJECT_ROOT" "/srv/git"
      ]
  }
