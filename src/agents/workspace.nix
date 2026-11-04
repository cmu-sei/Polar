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

# `nix-container-lib` is flake.nix's, which puts the platform version label on every image; `polarVersion` is the
# repository's VERSION.
{ pkgs, lib, crane, nix-container-lib, inputs, system, polarVersion, ... }:

let

    # Every binary is static, for musl on the architecture of `system`, as polar-tools always was
    # (polar-tools/package.nix): crane with the musl target, pkgsStatic's cc as linker and C compiler, OpenSSL linked
    # in. Each binary allocates with Scudo (docs/architecture/broker-baseline.md, "Decided").
    triple =
        if system == "aarch64-linux" then "aarch64-unknown-linux-musl"
        else "x86_64-unknown-linux-musl";

    # cargo's spelling of the triple in variable names
    tripleUnderscore = builtins.replaceStrings [ "-" ] [ "_" ] triple;
    tripleUpper = lib.toUpper tripleUnderscore;

    staticCC = pkgs.pkgsStatic.stdenv.cc;
    staticCCBin = "${staticCC}/bin/${staticCC.targetPrefix}cc";
    staticSsl = pkgs.pkgsStatic.openssl;

    # Scudo with our configuration (polar-scudo/config), built by nix-scudo: <dir>/<target>/libpolar_scudo.a, which
    # polar-scudo's build.rs links. The musl target is what every binary ships with; this system's gnu target is for
    # `cargo test` in the development shell and container, whose cargo builds for the host.
    hostTriple =
        if system == "aarch64-linux" then "aarch64-unknown-linux-gnu"
        else "x86_64-unknown-linux-gnu";
    scudoFor = p: inputs.nix-scudo.lib.mkScudo {
        pkgs = p;
        config = ./polar-scudo/config/custom_scudo_config.h;
        prefix = "polar_scudo_";
        name = "polar_scudo";
    };
    # Beside it, libpolar_mem.a: LLVM libc's memcpy and memmove (nixpkgs' LLVM source), which build.rs links whole, so
    # every binary calls them and not the libc's (docs/architecture/broker-baseline.md, "The fourth set"). musl's
    # x86_64 memcpy is `rep movsq` and a byte loop. memmove too: musl's x86_64 memmove jumps to __memcpy_fwd, in the
    # object of its memcpy, so a binary that took musl's memmove took its memcpy as well, a second definition. They are
    # freestanding (no libc), so one object serves the musl and the gnu target.
    llvmLibc = pkgs.llvmPackages.libc.src;
    mem = pkgs.runCommandCC "polar-mem" { nativeBuildInputs = [ pkgs.clang ]; } ''
        for f in memcpy memmove; do
          clang++ -std=c++17 -O3 -ffreestanding -fno-builtin -fno-exceptions -fno-rtti -fno-stack-protector -fPIC \
            -DLIBC_NAMESPACE=__llvm_libc_polar -DLIBC_COPT_PUBLIC_PACKAGING -I${llvmLibc}/libc \
            -c ${llvmLibc}/libc/src/string/$f.cpp -o llvm_libc_$f.o
        done
        mkdir -p $out/lib
        ar rcs $out/lib/libpolar_mem.a llvm_libc_memcpy.o llvm_libc_memmove.o
    '';
    scudo = pkgs.runCommand "polar-scudo-libs" { } ''
        for t in ${triple}:${scudoFor pkgs.pkgsStatic} ${hostTriple}:${scudoFor pkgs}; do
          d=$out/lib/polar/scudo/''${t%%:*}
          install -Dm644 ''${t#*:}/lib/libpolar_scudo.a $d/libpolar_scudo.a
          install -Dm644 ${mem}/lib/libpolar_mem.a $d/libpolar_mem.a
        done
    '';

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
        # Here and not in commonArgs: buildDepsOnly replaces the workspace's build scripts with dummies, so
        # polar-scudo's never reads it there, and in commonArgs a change to Scudo rebuilt every dependency.
        POLAR_SCUDO_LIB_DIR = "${scudo}/lib/polar/scudo";
        # The platform version, which polar-version embeds: crane's source is rooted here, so the repository's VERSION is
        # not in it, and polar-version's build.rs takes the contents from this variable instead. Here and not in
        # commonArgs for the same reason as the Scudo directory: the dependencies' build never reads it.
        POLAR_VERSION = polarVersion;
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

    softState = import ./soft-state/package.nix {
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

    buildOrchestrator = import ./build-orchestrator/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
      healthcheckDrv = healthcheck;
    };

    polarOperator = import ./polar-operator/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    polarCustody = import ./polar-custody/package.nix {
      inherit pkgs craneLib workspaceFileset nix-container-lib inputs system;
      crateArgs = individualCrateArgs;
    };

    polarTools = import ./polar-tools/package.nix {
      inherit pkgs crane workspaceFileset system;
      crateArgs = individualCrateArgs;
    };
in
{
        inherit scudo workspacePackages gitlabAgent cassini kubeAgent webAgent ociResolver scheduler jiraAgent gitAgent softState buildProcessor certIssuer nuInit gitServer polarGitServer polarInit polarOperator buildOrchestrator polarCustody polarTools healthcheck credentialsAgent secretHarness;
}
