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

# src/agents/soft-state/package.nix
#
# The state agent (docs/architecture/soft-state.md): its reader, soft-state-observer, and its writer,
# soft-state-processor, which asks every agent about its health and writes what it hears.
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
  observer = craneLib.buildPackage (crateArgs // {
    pname = "soft-state-observer";
    cargoExtraArgs = "--bin soft-state-observer --locked";
    src = workspaceFileset ./soft-state/observe;
  });

  processor = craneLib.buildPackage (crateArgs // {
    pname = "soft-state-processor";
    cargoExtraArgs = "--bin soft-state-processor --locked";
    src = workspaceFileset ./soft-state/process;
  });

  healthcheckInputs = inputs // {
    polar-healthcheck = {
      packages.${system}.default = healthcheckDrv;
    };
  };

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-observer.nix;
    extraDerivations = [ observer ];
  };

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ processor ];
  };
in
{
  inherit observer processor;
  observerImage = observerContainer.image;
  processorImage = processorContainer.image;
}
