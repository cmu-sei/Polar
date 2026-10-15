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

# src/containers/nu-init/package.nix
#
# Packages the polar-nu-init container image.
#
# This is a minimal nushell init container. Its sole responsibility is to
# execute /scripts/init.nu, which is mounted in by the pod spec at runtime.
# The container itself has no opinion about what the script does.
#
# The entrypoint wrapper is a tiny shell script that validates the mount
# exists and hands off to nushell.

{ pkgs
, nix-container-lib
, inputs
, system
, certIssuerClient
}:

let

  # Entrypoint: validate the script mount exists, then run it with nushell.
  # Fails loudly if /scripts/init.nu is not mounted — better than silently
  # succeeding with no init work done.
  nuInitEntrypoint = pkgs.writeShellApplication {
    name = "nu-init-entrypoint";

    runtimeInputs = [ pkgs.nushell pkgs.coreutils ];

    text = ''
      set -euo pipefail

      SCRIPT="/scripts/init.nu"

      if [[ ! -f "$SCRIPT" ]]; then
        echo "[nu-init] ERROR: $SCRIPT not found." >&2
        echo "[nu-init] Mount a ConfigMap containing init.nu at /scripts/" >&2
        exit 1
      fi

      echo "[nu-init] executing $SCRIPT..."
      exec nu "$SCRIPT"
    '';
  };

  nuInitContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ nuInitEntrypoint certIssuerClient ];
  };

in
{
  inherit nuInitEntrypoint;
  image = nuInitContainer.image;
}
