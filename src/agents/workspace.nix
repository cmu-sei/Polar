# TODO: A nix module that packages tests and contaienrizes the cargo workspace

{ pkgs, lib, crane, rust-overlay, ... }:

let

    craneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.nightly.latest.default);

    src = craneLib.cleanCargoSource ./.;

    # Common arguments for crane's build context
    commonArgs = {
        inherit src;
        strictDeps = true;

        buildInputs = [
        # Add additional build inputs here
        ];

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
      cargoExtraArgs = "--workspace --locked";
      src = workspaceFileset ./.;
    });

    # define a common uid/gid for use in the images
    commonUser = {
      name = "polar";
      uid = "1000";
      gid = "1000";
    };

    # Create passwd/group/shadow files
    etc = pkgs.runCommand "polar-etc" {
      buildInputs = [ pkgs.shadow ];
    } ''
      mkdir -p $out/etc $out/home/${commonUser.name}

      echo "${commonUser.name}:x:${commonUser.uid}:${commonUser.gid}::/home/${commonUser.name}:/bin/bash" > $out/etc/passwd
      echo "${commonUser.name}:x:${commonUser.gid}:" > $out/etc/group
      echo "${commonUser.name}:!x:::::::" > $out/etc/shadow
      chmod -R 755 $out/home/${commonUser.name}
    '';
    commonPaths = with pkgs; [
      # -- Basic Required Files --
      bash # Basic bash to run bare essential code
      glibcLocalesUtf8
      uutils-coreutils-noprefix # Essential GNU utilities (ls, cat, etc.)
      busybox
      etc
    ];
    cassini = import (workspaceRoot + /broker/package.nix) {
      inherit pkgs commonPaths craneLib  workspaceFileset cargoArtifacts commonUser;
      crateArgs = individualCrateArgs;
    };

    gitlabAgent = import (workspaceRoot + /gitlab/package.nix) {
      inherit pkgs commonPaths craneLib  workspaceFileset cargoArtifacts commonUser;
      crateArgs = individualCrateArgs;
    };

    kubeAgent = import ./kubernetes/package.nix {
      inherit pkgs commonPaths craneLib  workspaceFileset cargoArtifacts commonUser;
      crateARgs = individualCrateArgs;
    };

in
{
  inherit workspacePackages gitlabAgent cassini kubeAgent;
}
