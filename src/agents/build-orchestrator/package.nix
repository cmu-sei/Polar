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
# Licensed under a MIT (SEI)-style license, please see license.txt or contact
# permission@sei.cmu.edu for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

# src/agents/build-orchestrator/package.nix
#
# build-orchestrator and its image. The binary is built with crane from the fork's workspace like every other agent;
# the image has what the operator needs of an agent: the binary, and polar-healthcheck, which the liveness probe runs.
# (Public Polar's copy of this port was a workspace of its own and wrapped a cargo-built binary in image.nix; it is
# a member of this workspace now, so that route is not needed.)
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
  orchestrator = craneLib.buildPackage (crateArgs // {
    pname = "build-orchestrator";
    cargoExtraArgs = "--bin build-orchestrator --locked";
    src = workspaceFileset ./.;
    doCheck = false;
  });

  orchestratorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath    = ./container.nix;
    extraDerivations = [ orchestrator ];
  };
in
{
  inherit orchestrator;
  image = orchestratorContainer.image;
  # An S3-compatible store for the jobs' logs, for a cluster that has none.
  objectStoreImage = import ./object-store-image.nix { inherit pkgs; };
}
