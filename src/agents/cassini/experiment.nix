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

# src/agents/cassini/experiment.nix
#
# The allocator experiment's second set (docs/architecture/broker-baseline.md): the broker built six ways, each in the
# broker's own container definition, so a run differs from the control by the build alone.
#
#   v0  glibc, its own malloc (the reference)     v2  musl, mimalloc (the control: every Polar binary's build)
#   s0  musl, Scudo from nixpkgs' LLVM, upstream's default configuration
#   s1  s0 with the large-block cache to 4 MiB and the quarantine compiled out
#   s2  s1 with shared thread caches, a 1 s release interval and the futex condition variable
#   s3  s2 with every source compiled with the CRC instructions, so the chunk header's checksum is the instruction
#   s4  s3 with one thread cache a thread (s1's), not shared ones that lock on every allocation and free
#   m0  musl, smalloc as published
#
# The Scudo configurations are polar-scudo/configs/<name>; the allocator is a feature of the broker
# (cassini/broker/Cargo.toml). On the experiment branch only.
#
#   nix build .#cassiniExperiment.s1            # an image
{ pkgs
, crane
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, healthcheckDrv
, cassiniImage
, ...
}:
let
  binArgs = features:
    "--bin cassini-server --locked -p cassini-broker --no-default-features"
    + pkgs.lib.optionalString (features != "") " --features ${features}";

  scudoSrc = "${pkgs.llvmPackages_latest.compiler-rt.src}/compiler-rt/lib/scudo/standalone";

  musl = name: features: extra: craneLib.buildPackage (crateArgs // {
    pname = "cassini-server-${name}";
    cargoExtraArgs = binArgs features;
    src = workspaceFileset ./broker;
    doCheck = false;
  } // extra);

  # Scudo is C++: without the musl C++ compiler named for the target, cc compiles it with the host's, against glibc's
  # headers, and the link fails on glibc's symbols (__isoc23_strtol, __memset_chk).
  tripleUnderscore = builtins.replaceStrings [ "-" ] [ "_" ] crateArgs.CARGO_BUILD_TARGET;
  staticCC = pkgs.pkgsStatic.stdenv.cc;

  scudo = name: config: extra: musl name "alloc-scudo" ({
    SCUDO_SRC = scudoSrc;
    POLAR_SCUDO_CONFIG = config;
    "CXX_${tripleUnderscore}" = "${staticCC}/bin/${staticCC.targetPrefix}c++";
  } // extra);

  # The reference, on glibc as every binary was before the musl build: its own toolchain and dependencies.
  craneGlibc = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.nightly.latest.default.override {
    extensions = [ "rust-src" "rust-std" ];
  });
  glibcArgs = {
    pname = "cassini-server-v0";
    inherit (crateArgs) version;
    src = workspaceFileset ./broker;
    strictDeps = true;
    doCheck = false;
    cargoExtraArgs = binArgs "";
    nativeBuildInputs = [ pkgs.openssl pkgs.pkg-config pkgs.cmake ];
    PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
  };
  glibc = craneGlibc.buildPackage (glibcArgs // { cargoArtifacts = craneGlibc.buildDepsOnly glibcArgs; });

  imageOf = server: (nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath = ./container-cassini.nix;
    extraDerivations = [ server ];
  }).image;

  servers = {
    v0 = glibc;
    s0 = scudo "s0" "default" { };
    s1 = scudo "s1" "cache" { };
    s2 = scudo "s2" "lean" { };
    s3 = scudo "s3" "lean" { POLAR_SCUDO_CRC32 = "everywhere"; };
    s4 = scudo "s4" "exclusive" { POLAR_SCUDO_CRC32 = "everywhere"; };
    m0 = musl "m0" "alloc-smalloc" { };
  };
in
{
  inherit servers;
  # The control is the broker every bring-up runs, so it is that image.
  v2 = cassiniImage;
} // builtins.mapAttrs (_: imageOf) servers
