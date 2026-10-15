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
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/e2334448bd4bb6348a467244d474f907d3d0e36d/dhall/prelude.dhall
        sha256:b81e69ef2fe811bc853a8a9a0202c0af802f7cd53c78f95f67083bf3dceee86b

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
