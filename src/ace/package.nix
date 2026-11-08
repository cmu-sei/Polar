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

# src/ace/package.nix
{ pkgs
, lib
, system
, pyproject-nix
, uv2nix
, pyproject-build-systems
, nix-container-lib
, inputs
, healthcheckDrv
, ...
}:

let
  pythonEnv = import ./flake_modules/python-env.nix {
    inherit pyproject-nix uv2nix pyproject-build-systems;
  };
  pyEnv = pythonEnv { inherit pkgs lib system; root = ./.; };

  aceContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath    = ./container-ace.nix;
    extraDerivations = [ pyEnv.acePackage ];
  };
in
{
  inherit (pyEnv) venv venvNoCheck appRuntime acePackage;
  image = aceContainer.image;
}
