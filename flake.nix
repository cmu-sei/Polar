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
    rust-overlay.url = "github:oxalica/rust-overlay?rev=1ff8663cd75a11e61f8046c62f4dbb05d1907b44";
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

  };

  outputs = { self, nixpkgs, crane, rust-overlay, flake-utils, advisory-db, myNeovimOverlay, staticanalysis, dotacat, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        # overlays = [ (import rust-overlay) ];

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

        containers = import ./src/flake/containers.nix {
          inherit system pkgs rust-overlay staticanalysis dotacat myNeovimOverlay;
        };

        polarPkgs = import ./src/agents/workspace.nix {
        inherit pkgs lib crane rust-overlay;
        };

        # get certificates for mtls
        tlsCerts = pkgs.callPackage ./src/flake/gen-certs.nix { inherit pkgs; };
      in
      {
        packages = {
          inherit polarPkgs containers tlsCerts;
          default = polarPkgs.workspacePackages;
        };

        # devShells.default = devShell;
      });
}
