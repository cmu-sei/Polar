{
  description =
  "
  This flake represents the build configurations for the Polar project.
  It outputs binaries and container images for each service,
  as well as containerized development and test environments for ease of use.
  See documentation for additional details.
  ";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
    myNeovimOverlay.url                         = "github:daveman1010221/nix-neovim";
    myNeovimOverlay.inputs.nixpkgs.follows      = "nixpkgs";
    myNeovimOverlay.inputs.flake-utils.follows  = "flake-utils";
    staticanalysis.url                          = "github:daveman1010221/polar-static-analysis";
    staticanalysis.inputs.nixpkgs.follows       = "nixpkgs";
    staticanalysis.inputs.flake-utils.follows   = "flake-utils";
    staticanalysis.inputs.rust-overlay.follows  = "rust-overlay";
    dotacat.url                                 = "github:daveman1010221/dotacat-fast";
    dotacat.inputs.nixpkgs.follows              = "nixpkgs";
    nix-container-lib.url                       = "github:daveman1010221/nix-container-lib";
    nix-container-lib.inputs.nixpkgs.follows    = "nixpkgs";
    nix-container-lib.inputs.flake-utils.follows = "flake-utils";
  };
  outputs = { self, nixpkgs, crane, rust-overlay, flake-utils, advisory-db,
              myNeovimOverlay, staticanalysis, dotacat, nix-container-lib, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config = {
            cc = "clang";
          };
          documentation = {
            dev.enable = true;
            man = {
              man-db.enable     = true;
              generateCaches    = true;
            };
          };
          overlays = [
            rust-overlay.overlays.default
            myNeovimOverlay.overlays.default
          ];
        };
        inherit (pkgs) lib;

        # ---------------------------------------------------------------------------
        # Dev container — built via nix-container-lib
        # configPath uses builtins.path to copy the full source tree into one store
        # path so that relative Dhall imports resolve correctly in the sandbox.
        # ---------------------------------------------------------------------------
        container = nix-container-lib.lib.${system}.mkContainer {
          inherit system pkgs;
          inputs     = { inherit staticanalysis dotacat myNeovimOverlay rust-overlay; };
          configPath = pkgs.writeText "polar-container.dhall" (
            builtins.replaceStrings
              [ "PRELUDE_PATH" ]
              [ "${nix-container-lib}/dhall/prelude.dhall" ]
              (builtins.readFile ./src/flake/container.dhall)
          );
        };

        polarPkgs = import ./src/agents/workspace.nix {
          inherit pkgs lib crane rust-overlay;
        };
        commitMsgHooksPkg = import ./src/git-hooks/package.nix {
          inherit pkgs crane;
        };

        # TLS certificates — using polar's own gen-certs.nix
        tlsCerts = pkgs.callPackage ./src/flake/gen-certs.nix { inherit pkgs; };

      in
      {
        packages = {
          inherit tlsCerts;
          devContainer = container.image;
          default      = polarPkgs.workspacePackages;
        };
        devShells.default = container.devShell;
      });
}
