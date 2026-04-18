# src/containers/workspace.nix
#
# Container image definitions for Polar infrastructure support.
#
# This workspace is separate from src/agents/ because these containers are
# infrastructure concerns, not application agents. They support the deployment
# pipeline rather than implementing Polar's observability features.
#
# Currently:
#   nu-init     — minimal nushell init container, used wherever a pod needs
#                 nushell-based initialization logic at startup
#   git-server  — local dev git HTTP server (host-side only, not deployed
#                 into the cluster). Serves local working trees over HTTP
#                 so agents can observe local commits without pushing to GitHub.
#
# Adding a new container:
#   1. Create src/containers/<name>/
#   2. Write container.dhall + run `just render` to produce container.nix
#   3. Write package.nix following the nu-init pattern
#   4. Import it here and add it to the output attrset
#   5. Expose it in flake.nix under packages.<platform>
#   6. Add a build + load target to the root Justfile

{ pkgs
, lib
, nix-container-lib
, inputs
, system
}:

let
  nuInit = import ./nu-init/package.nix {
    inherit pkgs nix-container-lib inputs system;
  };

  gitServer = import ./git-server/package.nix {
    inherit pkgs nix-container-lib inputs system;
  };

in
{
  inherit nuInit gitServer;
}
