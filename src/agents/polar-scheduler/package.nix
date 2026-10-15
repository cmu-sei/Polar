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

# src/agents/polar-scheduler/package.nix
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
  processor = craneLib.buildPackage (crateArgs // {
    pname = "polar-scheduler";
    cargoExtraArgs = "--bin polar-scheduler --locked";
    src = workspaceFileset ./polar-scheduler/processor;
    doCheck = false;
  });

  observer = craneLib.buildPackage (crateArgs // {
    pname = "polar-scheduler-observer";
    cargoExtraArgs = "--bin polar-scheduler-observer --locked";
    src = workspaceFileset ./polar-scheduler/observer;
    doCheck = false;
  });

  healthcheckInputs = inputs // {
    polar-healthcheck = {
      packages.${system}.default = healthcheckDrv;
    };
  };

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ processor ];
  };

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-observer.nix;
    # The observer runs git to clone and pull the schedules repository.
    extraDerivations = [ observer pkgs.git ];
  };
in
{
  inherit processor observer;
  processorImage = processorContainer.image;
  observerImage  = observerContainer.image;
}
