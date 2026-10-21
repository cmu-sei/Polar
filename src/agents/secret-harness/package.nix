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

# src/agents/secret-harness/package.nix
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
  # One binary per workload: the role and the service name are compile-time constants
  # (src/settings.rs, src/supervisor.rs), so the owner and the intruder are two builds.
  build = pname: features: craneLib.buildPackage (crateArgs // {
    inherit pname;
    cargoExtraArgs = "--bin secret-harness --locked" + features;
    src = workspaceFileset ./.;
  });

  owner = build "secret-harness-owner" "";
  intruder = build "secret-harness-intruder" " --features intruder";

  container = configNixPath: binary: nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs configNixPath;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    extraDerivations = [ binary ];
  };
in
{
  inherit owner intruder;
  ownerImage = (container ./container-owner.nix owner).image;
  intruderImage = (container ./container-intruder.nix intruder).image;
}
