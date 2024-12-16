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
        # pkgs (which would require rebuidling anything else which uses rust).
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

          nativeBuildInputs = [
            pkgs.openssl
            pkgs.pkg-config            
          ];

          PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig";
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
            (craneLib.fileset.commonCargoSources ./consume)
            (craneLib.fileset.commonCargoSources ./observe)
            (craneLib.fileset.commonCargoSources ./common)
            (craneLib.fileset.commonCargoSources ./workspace-hack)
            (craneLib.fileset.commonCargoSources crate)
          ];
        };

        #build workspace derivation to be given as a default package
        agentPkgs = craneLib.buildPackage (individualCrateArgs // {
          pname = "gitlabAgent";
          cargoExtraArgs = "-p gitlab_agent -p gitlab_consumer";
          src = fileSetForCrate ./observe;
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
          cargoExtraArgs = "-p gitlab_agent"; #build the binaries and all its dependencies, including common
          src = fileSetForCrate ./observe;
        });
        gitlabConsumer = craneLib.buildPackage (individualCrateArgs // {
          pname = "gitlab_consumer";
          cargoExtraArgs = "-p gitlab_consumer"; 
          src = fileSetForCrate ./consume;
        });

        #get certificates
        tlsCerts = pkgs.callPackage ./scripts/gen-certs.nix { inherit pkgs; };
        #TODO: how to get *just* the client certificates and keys in the containers?

        #set up service environments
        observerEnv = pkgs.buildEnv {
              name = "image-root";
              paths =  [pkgs.bashInteractiveFHS pkgs.busybox gitlabObserver ];
              pathsToLink = [ "/bin" ];
        };

        consumerEnv = pkgs.buildEnv {
          name = "image-root";
          paths = [ pkgs.bashInteractiveFHS pkgs.busybox gitlabConsumer ];
          pathsToLink = [ "/bin" ];
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
          inherit gitlabObserver gitlabConsumer agentPkgs tlsCerts;
          default = agentPkgs;
          observerImage = pkgs.dockerTools.buildImage {
            name = "polar-gitlab-observer";
            tag = "latest";
            copyToRoot = [ observerEnv]; 

            #TODO: Run any other setup
            # runAsRoot = ''
            #   mkdir -p /data
            # '';

            config = {
              Cmd = [ "/app/observer-entrypoint" ];
              WorkingDir = "/app";
              Env = [ ]; #TODO: Populate env vars with info depending on environment
              # Volumes = {
              #   "/data" = { };
              # };
            };
         };
          consumerImage = pkgs.dockerTools.buildImage {
              name = "polar-gitlab-consumer";
              tag = "latest";
              copyToRoot = [consumerEnv tlsCerts];

              #TODO: Run any other setup
              # runAsRoot = ''
              #   mkdir -p /data
              # '';

              config = {
                Cmd = [ "/app/observer-entrypoint" ]; #TOOD: evaluate whether this is still the case
                WorkingDir = "/app";
                Env = [ ]; #TODO: Populate env vars with info depending on environment
                # Volumes = {
                #   "/data" = { };
                # };
              };
            };
          };
      });
}
