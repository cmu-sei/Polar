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

# The custody daemon (docs/architecture/key-custody.md): a native sidecar in every pod whose keys it holds.

{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
}:

let
  polarCustodyBin = craneLib.buildPackage (crateArgs // {
    pname = "polar-custody";
    cargoExtraArgs = "--bin polar-custody --locked";
    src = workspaceFileset ./.;
    doCheck = false;
  });

  polarCustodyContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ polarCustodyBin ];
  };

in
{
  inherit polarCustodyBin;
  image = polarCustodyContainer.image;
}
