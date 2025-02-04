{
  description = "This flake builds, tests, and runs static analysis on the Polar Gitlab Agent Workspace, outputting binaries and container images for each service.";
  #CAUTION: A single flake could build the entire project, after a while, it might become tough to maintain as we add agents, adapters, etc.
  #TODO: Explore ways to decompose this flake as it grows. Perhaps flake-parts can help https://github.com/hercules-ci/flake-parts

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay?rev=1ff8663cd75a11e61f8046c62f4dbb05d1907b44";
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


          # CMAKE="/bin/cmake";
          # CMAKE_MAKE_PROGRAM="/bin/make";
          # LIBCLANG_PATH = "${pkgs.llvmPackages.clang}/lib";
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

        fileSetForCrate = crate: lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            (craneLib.fileset.commonCargoSources ./broker)
            (craneLib.fileset.commonCargoSources ./gitlab/consume)
            (craneLib.fileset.commonCargoSources ./gitlab/observe)
            (craneLib.fileset.commonCargoSources ./gitlab/common)
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
          cargoExtraArgs = "--locked"; 
          src = ./broker;
        });

        # get certificates for mtls
        tlsCerts = pkgs.callPackage ../flake/gen-certs.nix { inherit pkgs; };

        ### set up environments
        
        #set up service environments
        observerEnv = pkgs.buildEnv {
          name = "image-root";
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

        observerConfig = pkgs.writeTextFile {
          name = "observerConfig";
          destination = "/.config/polar/observer_config.yaml";
          text = builtins.readFile ./observe/conf/observer_config.yaml;
        };

        consumerEnv = pkgs.buildEnv {
          name = "image-root";
          paths = [ pkgs.bashInteractiveFHS pkgs.busybox gitlabConsumer ];
          pathsToLink = [ 
            "/bin"
            "/etc/ssl/certs"
          ];
        };

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
        checks = {
          # Build the crates as part of `nix flake check` for convenience
          inherit gitlabObserver gitlabConsumer;

          # Run clippy (and deny all warnings) on the workspace source,
          # again, reusing the dependency artifacts from above.
          #
          # Note that this is done as a separate derivation so that
          # we can block the CI if there are issues here, but not
          # prevent downstream consumers from building our crate by itself.
          gitlabAgentclippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          gitlabAgentdoc = craneLib.cargoDoc (commonArgs // {
            inherit cargoArtifacts;
          });

          # Check formatting
          gitlabAgentfmt = craneLib.cargoFmt {
            inherit src;
          };

          gitlabAgentTomlFmt = craneLib.taploFmt {
            src = pkgs.lib.sources.sourceFilesBySuffices src [ ".toml" ];
            # taplo arguments can be further customized below as needed
            # taploExtraArgs = "--config ./taplo.toml";
          };

          # Audit dependencies
          gitlabAgentAudit = craneLib.cargoAudit {
            inherit src advisory-db;
          };

          # Audit licenses
          gitlabAgentDeny = craneLib.cargoDeny {
            inherit src;
          };

          # Run tests with cargo-nextest
          # Consider setting `doCheck = false` on other crate derivations
          # if you do not want the tests to run twice
          gitlabAgentnextest = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            partitions = 1;
            partitionType = "count";
          });

          # Ensure that cargo-hakari is up to date
          gitlabAgentHakari = craneLib.mkCargoDerivation {
            inherit src;
            pname = "hakari";
            cargoArtifacts = null;
            doInstallCargoArtifacts = false;

            buildPhaseCargoCommand = ''
              cargo hakari generate --diff  # workspace-hack Cargo.toml is up-to-date
              cargo hakari manage-deps --dry-run  # all workspace crates depend on workspace-hack
              cargo hakari verify
            '';

            nativeBuildInputs = [
              pkgs.cargo-hakari
            ];
          };
        };

        packages = {
          inherit gitlabObserver gitlabConsumer cassini agentPkgs tlsCerts;
          default = agentPkgs;
          observerImage = pkgs.dockerTools.buildImage {
            name = "polar-gitlab-observer";
            tag = "latest";
            copyToRoot = [ 
              observerEnv
              observerConfig
              "${tlsCerts}/ca_certificates"
              "${tlsCerts}/client"              
            ]; 

            # FIXME: certs get put in '/' which isn't bad but it's not great either, we'd rather have them in etc.
            # the buildEnv creates the /etc/ssl/certs path but it doesn't exist when these comamnds are ran.
            # extraCommands = ''
            #  mv ca_ * /etc/ssl/certs
            #  mv client_* /etc/ssl/certs
            # '';

            config = {
              Cmd = [ "/app/observer-entrypoint" ];
              WorkingDir = "/app";
              Env = [
                # The absolute file path to the client .p12 file. This is used by the Rust
                # binaries to auth with the broker via TLS.
                "TLS_CLIENT_KEY=/client_rabbitmq.p12"
                # If a password was set for the .p12 file, put it here.
                # "TLS_KEY_PASSWORD=somepassword"
                # The absolute file path to the ca_certificates.pem file created by TLS_GEN.
                # Used by the Rust binaries to auth with RabbitMQ via TLS.
                "TLS_CA_CERT=/ca_certificate.pem"
               ];
            };
          };
          consumerImage = pkgs.dockerTools.buildImage {
              name = "polar-gitlab-consumer";
              tag = "latest";
              copyToRoot = [consumerEnv tlsCerts
              "${tlsCerts}/ca_certificates"
              "${tlsCerts}/client"              
              ];

              config = {
                Cmd = [ "/app/observer-entrypoint" ]; #TOOD: evaluate whether this is still the case
                WorkingDir = "/app";
                Env = [
                  # The absolute file path to the client .p12 file. This is used by the Rust
                  # binaries to auth with the broker via TLS.
                  "TLS_CLIENT_KEY=/client_rabbitmq.p12"
                  # If a password was set for the .p12 file, put it here.
                  # "TLS_KEY_PASSWORD=somepassword"
                  # The absolute file path to the ca_certificates.pem file created by TLS_GEN.
                  # Used by the Rust binaries to auth with RabbitMQ via TLS.
                  "TLS_CA_CERT=/ca_certificate.pem"
                 ];
              };
            };
          
          cassiniImage = pkgs.dockerTools.buildImage {
            name = "cassini";
            tag = "latest";
            copyToRoot = [
              cassiniEnv
              "${tlsCerts}/ca_certificates"
              "${tlsCerts}/server"     
            ]; 
            config = {
              Cmd = [ "/broker/cassini" ];
              WorkingDir = "/app";
              Env = [ 
                "BIND_ADDR=0.0.0.0:8080"
                "TLS_CA_CERT=/ca_certificate.pem"
                "TLS_SERVER_CERT_CHAIN=/server_polar_certificate_chain.pem"
                "TLS_SERVER_KEY=broker/server_polar_key.pem"
              ];
            };
          };
          };
      });
}
