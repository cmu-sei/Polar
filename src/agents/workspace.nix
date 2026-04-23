{ pkgs, lib, crane, nix-container-lib, inputs, system, ... }:

let

    craneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.nightly.latest.default.override {
        extensions = [ "rust-src" "rust-std" ];
        targets = [ "x86_64-unknown-linux-gnu" ];
    });

    src = craneLib.cleanCargoSource ./.;

    # Common arguments for crane's build context
    commonArgs = {
        inherit src;
        strictDeps = true;

        buildInputs = [ ];

        nativeBuildInputs = [
        pkgs.openssl
        pkgs.pkg-config
        pkgs.cmake
        pkgs.libgcc
        pkgs.libclang
        ];

        PKG_CONFIG_PATH= "${pkgs.openssl.dev}/lib/pkgconfig";
    };

    # Build *just* the cargo dependencies (of the entire workspace),
    cargoArtifacts = craneLib.buildDepsOnly commonArgs;

    #Build arguments we want to pass to each crate
    individualCrateArgs = commonArgs // {
        inherit cargoArtifacts;
        inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
        # NB: we disable tests since we'll run them all via cargo-nextest
        doCheck = false;
    };

    # Path to your workspace root
    workspaceRoot = ./.;

    # List of all entries in the directory
    allEntries = builtins.readDir workspaceRoot;

    # Filter to only include subdirectories (not files, not hidden)
    subdirs = lib.attrsets.filterAttrs (name: type: type == "directory" && !(lib.hasPrefix "." name)) allEntries;

    # Turn each subdir into a fileset rooted in the workspace
    subdirFilesets = lib.mapAttrsToList (name: _: craneLib.fileset.commonCargoSources (workspaceRoot + "/${name}")) subdirs;

    # Combine all subdir filesets into one
    # Any other files needed by source code should be included here
    crateFileset = lib.fileset.unions (subdirFilesets ++ [
      ./Cargo.toml
      ./Cargo.lock
      ./gitlab/schema/src/gitlab.graphql
    ]);

    workspaceFileset = crate: lib.fileset.toSource {
      root = ./.;
      fileset = crateFileset;
    };

    # build workspace derivation to be given as a default package
    workspacePackages = craneLib.buildPackage (individualCrateArgs // {
      pname = "polar";
      cargoExtraArgs = "--workspace --locked --exclude logger --exclude policy-config --exclude config-ops";
      src = workspaceFileset ./.;
    });

    cassini = import (workspaceRoot + /cassini/package.nix) {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    gitlabAgent = import (workspaceRoot + /gitlab/package.nix) {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    kubeAgent = import ./kubernetes/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    webAgent = import ./openapi/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    provenance = import ./provenance/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    scheduler = import ./polar-scheduler/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    jiraAgent = import ./jira/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    gitAgent = import ./git/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    buildOrchestrator = import ./build-orchestrator/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };
in
{
  inherit workspacePackages gitlabAgent cassini kubeAgent webAgent provenance scheduler jiraAgent gitAgent buildOrchestrator;
}
