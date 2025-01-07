{
  description = "This flake builds, tests, and runs static analysis on the Polar Gitlab Agent Workspace, outputting binaries and container images for each service.";
  #CAUTION: A single flake could build the entire project, after a while, it might become tough to maintain as we add agents, adapters, etc.
  #TODO: Explore ways to decompose this flake as it grows. Perhaps flake-parts can help https://github.com/hercules-ci/flake-parts
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay?rev=260ff391290a2b23958d04db0d3e7015c8417401";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    rust-overlay.inputs.flake-utils.follows = "flake-utils";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, crane, rust-overlay, flake-utils, advisory-db, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        
        inherit (pkgs) lib;

        # NB: we don't need to overlay our custom toolchain for the *entire*
        # pkgs (which would require rebuilding anything else which uses rust).
        # Instead, we just want to update the scope that crane will use by appending
        # our specific toolchain there.
        craneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.stable.latest.default.override {
          extensions = ["rust-src"];
          targets = ["x86_64-unknown-linux-gnu" "x86_64-apple-darwin" ];
        });

        src = craneLib.cleanCargoSource ./.;

        # Common arguments can be set here to avoid repeating them later
        commonArgs = {
          inherit src;
          strictDeps = true;

          buildInputs = [
            # Add additional build inputs here
          ] ++ lib.optionals pkgs.stdenv.isDarwin [
            # Additional darwin specific inputs can be set here
            pkgs.libiconv
          ];

          # TODO: use FIPS compliant openssl
          # REFERENCE: https://github.com/MaxfieldKassel/nix-flake-openssl-fips
          nativeBuildInputs = [
            pkgs.openssl
            pkgs.pkg-config            
          ];

          PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig";
        };

        # Build arguments we want to pass to each crate
        individualCrateArgs = commonArgs // {
          inherit cargoArtifacts;
          inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
          # NB: we disable tests since we'll run them all via cargo-nextest
          doCheck = false;
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual crate itself, reusing the dependency
        # artifacts from above.
        cassini = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--locked"; 
        });

        #set up service environments
        cassiniEnv = pkgs.buildEnv {
          name = "image-root";
          paths =  [ 
            pkgs.bashInteractiveFHS 
            pkgs.busybox 
            cassini
          ];

          pathsToLink = [ 
            "/bin"
            "/etc/ssl/certs"
          ];
        };

      in
      {
        packages = {
          inherit cassini;
          default = cassini;
          brokerImage = pkgs.dockerTools.buildImage {
            name = "cassini";
            tag = "latest";
            copyToRoot = [ cassiniEnv ]; 
            config = {
              Cmd = [ "/app/observer-entrypoint" ];
              WorkingDir = "/app";
              Env = [ ];
            };
          };
        };
      
      }
      );
}
