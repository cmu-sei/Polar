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

# src/agents/cassini/package.nix
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
  cassini = craneLib.buildPackage (crateArgs // {
    pname = "cassini-server";
    cargoExtraArgs = "--bin cassini-server --locked";
    src = workspaceFileset ./broker;
    doCheck = false;
  });

  client = craneLib.buildPackage (crateArgs // {
    pname = "cassini-client";
    cargoExtraArgs = "--bin cassini-client --locked";
    src = workspaceFileset ./client;
    doCheck = false;
  });

  harnessProducer = craneLib.buildPackage (crateArgs // {
    pname = "harness-producer";
    cargoExtraArgs = "--bin harness-producer --locked";
    src = workspaceFileset ./test/producer;
    doCheck = false;
  });

  harnessSink = craneLib.buildPackage (crateArgs // {
    pname = "harness-sink";
    cargoExtraArgs = "--bin harness-sink --locked";
    src = workspaceFileset ./test/sink;
    doCheck = false;
  });

  cassiniContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath    = ./container-cassini.nix;
    extraDerivations = [ cassini ];
  };

  producerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath    = ./container-harness-producer.nix;
    extraDerivations = [ harnessProducer ];
  };

  sinkContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath    = ./container-harness-sink.nix;
    extraDerivations = [ harnessSink ];
  };
in
{
  inherit cassini client harnessProducer harnessSink;
  cassiniImage  = cassiniContainer.image;
  producerImage = producerContainer.image;
  sinkImage     = sinkContainer.image;
}
