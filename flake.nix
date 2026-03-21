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
    myNeovimOverlay.url                           = "github:daveman1010221/nix-neovim";
    myNeovimOverlay.inputs.nixpkgs.follows        = "nixpkgs";
    myNeovimOverlay.inputs.flake-utils.follows    = "flake-utils";
    staticanalysis.url                            = "github:daveman1010221/polar-static-analysis";
    staticanalysis.inputs.nixpkgs.follows         = "nixpkgs";
    staticanalysis.inputs.flake-utils.follows     = "flake-utils";
    staticanalysis.inputs.rust-overlay.follows    = "rust-overlay";
    dotacat.url                                   = "github:daveman1010221/dotacat-fast";
    dotacat.inputs.nixpkgs.follows                = "nixpkgs";
    nix-container-lib.url                         = "github:daveman1010221/nix-container-lib";
    nix-container-lib.inputs.nixpkgs.follows      = "nixpkgs";
    nix-container-lib.inputs.flake-utils.follows  = "flake-utils";
    pi-agent-rust-nix.url                         = "github:daveman1010221/pi-agent-rust-nix";
    pi-agent-rust-nix.inputs.nixpkgs.follows      = "nixpkgs";
    pi-agent-rust-nix.inputs.rust-overlay.follows = "rust-overlay";
  };
  outputs = { self, nixpkgs, crane, rust-overlay, flake-utils, advisory-db,
              myNeovimOverlay, staticanalysis, dotacat, nix-container-lib, pi-agent-rust-nix, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config = {
            cc = "clang";
            allowUnfree = true;   # CUDA is unfree, needed alongside cudaSupport
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
          inputs     = { inherit staticanalysis dotacat rust-overlay; };
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

        piAgent = pi-agent-rust-nix.packages.${system}.default;

        pkgsCuda = import nixpkgs {
          inherit system;
          config = {
            cc = "clang";
            allowUnfree = true;
            cudaSupport = true;
          };
          overlays = [
            rust-overlay.overlays.default
            myNeovimOverlay.overlays.default
          ];
        };

        mkAgentContainer = llamaCppPkg: extraPkgs: nix-container-lib.lib.${system}.mkContainer {
          inherit system pkgs;
          inputs = { inherit staticanalysis dotacat rust-overlay;
            piAgent  = { packages.${system} = { default = piAgent; }; };
            llamaCpp = { packages.${system} = { default = llamaCppPkg; }; };
            cudaLibs = { packages.${system} = { default = extraPkgs; }; };
          };
          configPath = pkgs.writeText "polar-agent-container.dhall" (
            builtins.replaceStrings
              [ "PRELUDE_PATH" ]
              [ "${nix-container-lib}/dhall/prelude.dhall" ]
              (builtins.readFile ./src/flake/agent-container.dhall)
          );
        };
      in
      {
        packages = {
          inherit polarPkgs tlsCerts;
          devContainer          = container.image;
          agentContainer        = (mkAgentContainer pkgs.llama-cpp pkgs.stdenv.cc).image;
          agentContainerRocm    = (mkAgentContainer pkgs.llama-cpp-rocm pkgs.stdenv.cc).image;
          agentContainerVulkan  = (mkAgentContainer pkgs.llama-cpp-vulkan pkgs.stdenv.cc).image;
          agentContainerNvidia  = (mkAgentContainer pkgsCuda.llama-cpp pkgsCuda.cudaPackages.cuda_cudart).image;
          piAgent        = pkgs.callPackage ./src/flake/pi-agent.nix {
            inherit (pkgs) lib fetchFromGitHub;
            rust-bin = pkgs.rust-bin;
          };
          default = polarPkgs.workspacePackages;
        };
        devShells.default = (import ./src/flake/containers.nix {
          inherit system pkgs rust-overlay staticanalysis dotacat myNeovimOverlay;
        }).devShells.default;
      });
}
