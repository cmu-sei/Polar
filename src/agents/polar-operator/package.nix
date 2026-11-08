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

# src/agents/polar-operator/package.nix
#
# The operator and its image. It runs as a Deployment (see src/deploy/types/operator.dhall), so it
# needs no health file or probe; it takes its arguments (`run`) from the Deployment.
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
}:

let
  operator = craneLib.buildPackage (crateArgs // {
    pname = "polar-operator";
    cargoExtraArgs = "--bin polar-operator --locked";
    src = workspaceFileset ./.;
    doCheck = false;
  });

  operatorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ operator ];
  };

in
{
  inherit operator;
  image = operatorContainer.image;
}
