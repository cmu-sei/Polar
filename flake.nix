{
  description = "This flake builds, tests, and runs static analysis on the Polar Gitlab Agent Workspace, outputting binaries and container images for each service.";
  #CAUTION: A single flake could build the entire project, after a while, it might become tough to maintain as we add agents, adapters, etc.
  #TODO: Explore ways to decompose this flake as it grows. Perhaps flake-parts can help https://github.com/hercules-ci/flake-parts

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay?rev=1ff8663cd75a11e61f8046c62f4dbb05d1907b44";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, crane, rust-overlay, flake-utils, advisory-db }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        inherit (pkgs) lib;

        # Define the devShell
        devShell = pkgs.mkShell {
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];

          shellHook = ''
            export OPENSSL_DIR="${pkgs.openssl.dev}"
            export OPENSSL_LIB_DIR="${pkgs.openssl.out}/lib"
            export OPENSSL_INCLUDE_DIR="${pkgs.openssl.dev}/include"
            export PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig"
          '';
        };

        polarPkgs = import ./src/agents/workspace.nix {
        inherit pkgs lib crane rust-overlay;
        };

        # get certificates for mtls
        tlsCerts = pkgs.callPackage ./src/flake/gen-certs.nix { inherit pkgs; };
      in
      {
        packages = {
          inherit polarPkgs tlsCerts;
          default = polarPkgs.workspacePackages;
        };

        devShells.default = devShell;
      });
}
