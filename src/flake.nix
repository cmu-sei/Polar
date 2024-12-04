{
  description = "Build the Polar workspace";
  #CAUTION: This flake should build the entire project, after a while, this flake might become tough to maintain as we add agents, adapters, etc.
  #TODO: Explore ways to decompose this flake as it grows. Perhaps flake-parts can help https://github.com/hercules-ci/flake-parts

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane.url = "github:ipetkov/crane";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };

    flake-utils.url = "github:numtide/flake-utils";

    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, crane, fenix, flake-utils, advisory-db, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        inherit (pkgs) lib;

        craneLib = crane.mkLib pkgs;
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

          # Additional environment variables can be set directly
          # MY_CUSTOM_VAR = "some value";
        };

        craneLibLLvmTools = craneLib.overrideToolchain
          (fenix.packages.${system}.complete.withComponents [
            "cargo"
            "llvm-tools"
            "rustc"
          ]);

        # Build *just* the cargo dependencies (of the entire workspace),
        # so we can reuse all of that work (e.g. via cachix) when running in CI
        # It is *highly* recommended to use something like cargo-hakari to avoid
        # cache misses when building individual top-level-crates
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

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
            (craneLib.fileset.commonCargoSources ./agents/gitlab/consume)
            (craneLib.fileset.commonCargoSources ./agents/gitlab/observe)
            (craneLib.fileset.commonCargoSources ./agents/gitlab/common)
            (craneLib.fileset.commonCargoSources ./workspace-hack)
            (craneLib.fileset.commonCargoSources crate)
          ];
        };

        # Build the top-level crates of the workspace as individual derivations.
        # This allows consumers to only depend on (and build) only what they need.
        # Though it is possible to build the entire workspace as a single derivation,
        # so this is left up to you on how to organize things
        #
        # TODO: Decide whether we want to keep each crate in the same derivation or not.
        # For example, we could group our crates by the service they're intended for, or we could serve each one individually.
        # Note that the cargo workspace must define `workspace.members` using wildcards,
        # otherwise, omitting a crate (like we do below) will result in errors since
        # cargo won't be able to find the sources for all members.
        
        gitlabObserver = craneLib.buildPackage (individualCrateArgs // {
          pname = "gitlab_agent";
          cargoExtraArgs = "-p gitlab_agent"; #build the binaries and all its dependencies, including common
          src = fileSetForCrate ./agents/gitlab/observe;
        });
        gitlabConsumer = craneLib.buildPackage (individualCrateArgs // {
          pname = "gitlab_consumer";
          cargoExtraArgs = "-p gitlab_consumer"; 
          src = fileSetForCrate ./agents/gitlab/consume;
        });
        #
        # commonGitlabLib = craneLib.buildPackage (individualCrateArgs // {
        #   pname = "common";
        #   src = fileSetForCrate ./agents/gitlab/common;
        # });

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

        #TODO: Investigate how we can build and apply checks to the whole workspace, but still distribute each crate individually.
        #TODO: We have to specify a default package for this flake, determine the best value for this.
        packages = {
          inherit gitlabObserver gitlabConsumer;
          default = gitlabObserver;
        } // lib.optionalAttrs (!pkgs.stdenv.isDarwin) {
          gitlabAgentcoverage = craneLibLLvmTools.cargoLlvmCov (commonArgs // {
            inherit cargoArtifacts;
          });
        };

        apps = {
          gitlabConsumer = flake-utils.lib.mkApp {
            drv = gitlabConsumer;
          };
          gitlabObserver = flake-utils.lib.mkApp {
            drv = gitlabObserver;
          };
        };
      });
}
