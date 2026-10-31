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

# src/agents/polar-tools/package.nix
#
# The standalone Polar tools, built as STATIC binaries for the architecture of `system`
# (musl, with OpenSSL linked in), so a binary copied out of the image runs on any Linux of
# that architecture: on the host, in the dev container, in a minimal container. These tools
# are always built this way; there is no dynamic build of them.
#
#   polar-sign          sign, seal and push configuration
#   polar-admin-grant   provision and claim accounts, bootstrap the CA
#   cassini-c2          ask the broker to shut down
#   cassini-client      the broker's client, for applications that are not written in Rust
#   user-cert-fetcher   self-service user certificate issuance
#   cert-client, cert-issuer-setup   the local PKI tools
#
# Outputs:
#   tools   $out/bin/<tool>: what a developer or the dev container puts on its PATH
#   image   an image holding the same files, as real files in /opt/polar/bin, so that
#           `nu infra/tools.nu install <dir>` can copy them out with skopeo and tar
{ pkgs
, crane
, crateArgs
, workspaceFileset
, system
, ...
}:
let
  triple =
    if system == "aarch64-linux" then "aarch64-unknown-linux-musl"
    else "x86_64-unknown-linux-musl";

  # cargo's spelling of the triple in variable names
  tripleUnderscore = builtins.replaceStrings [ "-" ] [ "_" ] triple;
  tripleUpper = pkgs.lib.toUpper tripleUnderscore;

  staticCC = pkgs.pkgsStatic.stdenv.cc;
  staticCCBin = "${staticCC}/bin/${staticCC.targetPrefix}cc";
  staticSsl = pkgs.pkgsStatic.openssl;

  craneStatic = (crane.mkLib pkgs).overrideToolchain (p:
    p.rust-bin.nightly.latest.default.override {
      extensions = [ "rust-src" "rust-std" ];
      targets = [ triple ];
    });

  packages = [
    "polar-sign"
    "polar-admin-grant"
    "cassini-c2"
    "cassini-client"
    "user-cert-fetcher"
    "cert-client"
  ];

  staticArgs = {
    pname = "polar-tools";
    inherit (crateArgs) version;
    src = workspaceFileset ./.;
    strictDeps = true;
    doCheck = false;
    cargoExtraArgs = "--locked " + pkgs.lib.concatMapStringsSep " " (p: "-p ${p}") packages;

    CARGO_BUILD_TARGET = triple;
    CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
    "CARGO_TARGET_${tripleUpper}_LINKER" = staticCCBin;
    "CC_${tripleUnderscore}" = staticCCBin;

    OPENSSL_STATIC = "1";
    OPENSSL_LIB_DIR = "${staticSsl.out}/lib";
    OPENSSL_INCLUDE_DIR = "${staticSsl.dev}/include";

    nativeBuildInputs = [ pkgs.pkg-config pkgs.cmake staticCC ];
  };

  cargoArtifacts = craneStatic.buildDepsOnly staticArgs;

  # The Scudo directory in the package's build only, as workspace.nix has it: the dependencies' build never reads it.
  tools = craneStatic.buildPackage (staticArgs // { inherit cargoArtifacts; inherit (crateArgs) POLAR_SCUDO_LIB_DIR POLAR_VERSION; });

  image = pkgs.dockerTools.buildLayeredImage {
    name = "polar-tools";
    tag = "latest";
    # Real files, not links into /nix/store, so that extracting the layer yields the tools.
    extraCommands = ''
      mkdir -p opt/polar/bin
      cp ${tools}/bin/* opt/polar/bin/
    '';
    config = {
      Labels = {
        "org.opencontainers.image.title" = "polar-tools";
        # The platform version (flake.nix, "The platform version"); never the commit.
        "org.opencontainers.image.version" = crateArgs.POLAR_VERSION;
        "org.opencontainers.image.description" =
          "Static Polar command-line tools in /opt/polar/bin; copy them out, there is nothing to run";
      };
    };
  };
in
{
  inherit tools image;
}
