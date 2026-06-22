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
