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

        # NB: we don't need to overlay our custom toolchain for the *entire*
        # pkgs (which would require rebuidling anything else which uses rust).
        # Instead, we just want to update the scope that crane will use by appending
        # our specific toolchain there.
        craneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.nightly."2025-01-06".default);

        src = craneLib.cleanCargoSource ./.;

        # Common arguments can be set here to avoid repeating them later
        commonArgs = {
          inherit src;
          strictDeps = true;

          buildInputs = [
            # Add additional build inputs here
          ];

          # TOOD: use FIPS compliant openssl
          # REFERENCE: https://github.com/MaxfieldKassel/nix-flake-openssl-fips
          nativeBuildInputs = [
            pkgs.openssl
            pkgs.pkg-config
            pkgs.cmake
            pkgs.libgcc            
            pkgs.libclang
          ] ++ lib.optionals pkgs.stdenv.isDarwin [
            # Additional darwin specific inputs can be set here
            pkgs.llvmPackages_19.stdenv
            pkgs.llvmPackages_19.libcxxClang
            pkgs.libiconv
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.CoreFoundation
          ];

          PKG_CONFIG_PATH= "${pkgs.openssl.dev}/lib/pkgconfig";
        };

        # Build *just* the cargo dependencies (of the entire workspace),
        # so we can reuse all of that work (e.g. via cachix) when running in CI
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        #Build arguments we want to pass to each crate
        individualCrateArgs = commonArgs // {
          inherit cargoArtifacts;
          inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
          # NB: we disable tests since we'll run them all via cargo-nextest
          doCheck = false;
        };

        # CAUTION! This represents crane's understanding of our cargo workspace.
        # Whenever new crates are added/removed from the workspace, the change should be reflected here as well.
        fileSetForCrate = crate: lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./gitlab/schema/src/gitlab.graphql
            (craneLib.fileset.commonCargoSources ./broker)
            (craneLib.fileset.commonCargoSources ./policy-config)
  	        (craneLib.fileset.commonCargoSources ./lib)
            (craneLib.fileset.commonCargoSources ./gitlab/consume)
            (craneLib.fileset.commonCargoSources ./gitlab/observe)
            (craneLib.fileset.commonCargoSources ./gitlab/common)
            (craneLib.fileset.commonCargoSources ./gitlab/query)
            (craneLib.fileset.commonCargoSources ./gitlab/schema)
            (craneLib.fileset.commonCargoSources ./workspace-hack)
            (craneLib.fileset.commonCargoSources crate)
          ];
        };

        # build workspace derivation to be given as a default package
        agentPkgs = craneLib.buildPackage (individualCrateArgs // {
          pname = "gitlabAgent";
          cargoExtraArgs = "--workspace --locked";
          src = fileSetForCrate ./.;
        });

        # Build the top-level crates of the workspace as individual derivations.
        # This allows consumers to only depend on (and build) only what they need.
        # Though it is possible to build the entire workspace as a single derivation,
        # so this is left up to you on how to organize things
        #
        # For example, we could group our crates by the service they're intended for, or we could serve each one individually.
        # Note that the cargo workspace must define `workspace.members` using wildcards,
        # otherwise, omitting a crate will result in errors since
        # cargo won't be able to find the sources for all members.
        
        gitlabObserver = craneLib.buildPackage (individualCrateArgs // {
          pname = "gitlab_agent";
          cargoExtraArgs = "--locked"; #build the binaries and all its dependencies, including common
          src = fileSetForCrate ./gitlab/observe;
        });
        gitlabConsumer = craneLib.buildPackage (individualCrateArgs // {
          pname = "gitlab_consumer";
          cargoExtraArgs = "--locked"; 
          src = fileSetForCrate ./gitlab/consume;
        });

        cassini = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          cargoExtraArgs = "--locked -p cassini"; 
          src = fileSetForCrate ./broker;
          # Disable tests for now, We'll run them later with env vars and TlsCerts
          doCheck = false;
        });

        # get certificates for mtls
        tlsCerts = pkgs.callPackage ../flake/gen-certs.nix { inherit pkgs; };

        ### set up environments
        
        #set up service environments
        observerEnv = pkgs.buildEnv {
          name = "gitlab-observer-env";
          paths =  [ 
            pkgs.bashInteractiveFHS 
            pkgs.busybox 
            gitlabObserver
          ];
          
          pathsToLink = [ 
            "/bin"
            "/etc/ssl/certs"
          ];
        };

        consumerEnv = pkgs.buildEnv {
          name = "gitlab-consumer-env";
          paths = [ pkgs.bashInteractiveFHS pkgs.busybox gitlabConsumer ];
          pathsToLink = [ 
            "/bin"
            "/etc/ssl/certs"
          ];
        };

        cassiniEnv = pkgs.buildEnv {
          name = "cassini-env";
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
          inherit gitlabObserver gitlabConsumer cassini agentPkgs tlsCerts;
          default = agentPkgs;

          observerImage = pkgs.dockerTools.buildImage {
            name = "polar-gitlab-observer";
            tag = "0.1.0";
            copyToRoot = [ observerEnv pkgs.cacert ]; 

            config = {
              Cmd = [ "gitlab-observer" ];
              WorkingDir = "/";
              Env = [
                "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                "SSL_CERT_DIR=/etc/ssl/certs"
              ];
            };
          };
          consumerImage = pkgs.dockerTools.buildImage {
              name = "polar-gitlab-consumer";
              tag = "0.1.0";
              copyToRoot = [ consumerEnv ];

              config = {
                Cmd = [ "gitlab-consumer" ];
                WorkingDir = "/";
                Env = [ ];
              };
            };
          
          cassiniImage = pkgs.dockerTools.buildImage {
            name = "cassini";
            tag = "0.1.0";
            copyToRoot = [
              cassiniEnv pkgs.cacert 
            ]; 
            config = {
              Cmd = [ "cassini-server" ];
              WorkingDir = "/app";
              Env = [ 
                "CASSINI_BIND_ADDR=0.0.0.0:8080"
              ];
            };
          };
        };

        devShells.default = devShell;
      });
}

