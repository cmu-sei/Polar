# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

{ pkgs, lib, crane, nix-container-lib, inputs, system, ... }:

let

    # Every binary is static, for musl on the architecture of `system`, as polar-tools always was
    # (polar-tools/package.nix): crane with the musl target, pkgsStatic's cc as linker and C compiler, OpenSSL linked
    # in. Each binary allocates with mimalloc (docs/architecture/broker-baseline.md, "musl and the allocators").
    triple =
        if system == "aarch64-linux" then "aarch64-unknown-linux-musl"
        else "x86_64-unknown-linux-musl";

    # cargo's spelling of the triple in variable names
    tripleUnderscore = builtins.replaceStrings [ "-" ] [ "_" ] triple;
    tripleUpper = lib.toUpper tripleUnderscore;

    staticCC = pkgs.pkgsStatic.stdenv.cc;
    staticCCBin = "${staticCC}/bin/${staticCC.targetPrefix}cc";
    staticSsl = pkgs.pkgsStatic.openssl;

    craneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.nightly.latest.default.override {
        extensions = [ "rust-src" "rust-std" ];
        targets = [ triple ];
    });

    src = craneLib.cleanCargoSource ./.;

    # Common arguments for crane's build context
    commonArgs = {
        inherit src;
        strictDeps = true;

        buildInputs = [ ];

        nativeBuildInputs = [
        pkgs.pkg-config
        pkgs.cmake
        staticCC
        ];

        CARGO_BUILD_TARGET = triple;
        CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
        "CARGO_TARGET_${tripleUpper}_LINKER" = staticCCBin;
        "CC_${tripleUnderscore}" = staticCCBin;

        OPENSSL_STATIC = "1";
        OPENSSL_LIB_DIR = "${staticSsl.out}/lib";
        OPENSSL_INCLUDE_DIR = "${staticSsl.dev}/include";
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
      ./polar-scudo/configs
    ]);

    workspaceFileset = crate: lib.fileset.toSource {
      root = ./.;
      fileset = crateFileset;
    };

    # build workspace derivation to be given as a default package
    workspacePackages = craneLib.buildPackage (individualCrateArgs // {
      pname = "polar";
      cargoExtraArgs = "--workspace --locked --exclude logger ";
      src = workspaceFileset ./.;
    });

    certIssuer = import (workspaceRoot + /cert-issuer/package.nix) {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    cassini = import (workspaceRoot + /cassini/package.nix) {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    gitlabAgent = import (workspaceRoot + /gitlab/package.nix) {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    kubeAgent = import ./kubernetes/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    webAgent = import ./openapi/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    buildProcessor = import ./build-processor/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    ociResolver = import ./resolver/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    scheduler = import ./polar-scheduler/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    jiraAgent = import ./jira/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    gitAgent = import ./git/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    credentialsAgent = import ./credentials-agent/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    polarGitServer = import ./polar-git-server/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    secretHarness = import ./secret-harness/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    nuInit = import ./nu-init/package.nix {
      inherit pkgs nix-container-lib inputs system;
      certIssuerClient = certIssuer.client;
    };

    gitServer = import ./git-server/package.nix {
      inherit pkgs nix-container-lib inputs system;
    };

    healthcheck = craneLib.buildPackage (individualCrateArgs // {
      pname = "polar-healthcheck";
      cargoExtraArgs = "--package polar-healthcheck --locked";
      src = workspaceFileset ./.;
    });

    polarInit = import ./polar-init/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    polarCustody = import ./polar-custody/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    # The allocator experiment's brokers (cassini/experiment.nix), on its branch only.
    cassiniExperiment = import ./cassini/experiment.nix {
      inherit pkgs crane craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
      cassiniImage = cassini.cassiniImage;
    };

    polarTools = import ./polar-tools/package.nix {
      inherit pkgs crane workspaceFileset system;
      crateArgs = individualCrateArgs;
    };
in
{
        inherit workspacePackages gitlabAgent cassini kubeAgent webAgent ociResolver scheduler jiraAgent gitAgent buildProcessor certIssuer nuInit gitServer polarGitServer polarInit polarCustody polarTools healthcheck credentialsAgent secretHarness cassiniExperiment;
}
