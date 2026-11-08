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

# src/agents/polar-git-server/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, healthcheckDrv
, ...
}:

let
  # The trees the server commits as the first commit of a repository it finds empty, one directory
  # per repository, taken from infra/seeds.
  seeds = pkgs.runCommand "polar-git-server-seeds" { } ''
    cp -r ${../../../infra/seeds} $out
  '';

  server = craneLib.buildPackage (crateArgs // {
    pname = "polar-git-server";
    cargoExtraArgs = "--bin polar-git-server --locked";
    src = workspaceFileset ./server;
    # The binary's default seed root. Having the path in the binary is also what puts the seeds
    # in the image: the container only links bin/ from its packages, but it carries each one's
    # whole closure, and a store path in the binary is a reference.
    POLAR_GIT_SERVER_BUILT_SEED_ROOT = "${seeds}";
  });

  container = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath = ./container.nix;
    # The server runs git (upload-pack, receive-pack) for every request.
    extraDerivations = [ server pkgs.git ];
  };
in
{
  inherit server seeds;
  image = container.image;
}
