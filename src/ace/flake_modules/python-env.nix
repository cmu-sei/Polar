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

# flake_modules/python-env.nix
{ pyproject-nix, uv2nix, pyproject-build-systems, ... }:
{ pkgs, lib, system, root, ... }:
let
  python = pkgs.python313;
  workspace = uv2nix.lib.workspace.loadWorkspace { workspaceRoot = ./.; };
  overlay   = workspace.mkPyprojectOverlay { sourcePreference = "wheel"; };
  baseSet = pkgs.callPackage pyproject-nix.build.packages { inherit python; };
  pythonSet = baseSet.overrideScope (lib.composeManyExtensions [
    pyproject-build-systems.overlays.default
    overlay
  ]);
  depsAll  = workspace.deps.default;
  depsOnly = builtins.removeAttrs depsAll [ "ace-visualization-prototype" ];
  extraPkgs = [
    "dash"
    "dash-bootstrap-components"
    "neo4j"
    "numpy"
    "plotly"
    "polars"
  ];
  venvDeps = depsOnly
    // builtins.listToAttrs (map (n: { name = n; value = [ ]; }) extraPkgs);
  venv = pythonSet.mkVirtualEnv "ace-env" venvDeps;
  venvNoCheck = venv.overrideAttrs (_: {
    dontCheckForBrokenSymlinks = true;
  });
  src = lib.cleanSourceWith {
    src = root;
    filter = path: type:
      let b = baseNameOf path;
      in !(
        lib.hasInfix "/.git/" path ||
        lib.hasInfix "/.direnv/" path ||
        lib.hasInfix "/.venv/" path ||
        b == "neo4j_data" ||
        b == "flake.lock.save"
      );
  };

  # appRuntime: just the app source tree, copied verbatim into its own
  # store path under $out/app. No /bin, no wrapper scripts -- those live
  # in acePackage below, which references this path directly.
  appRuntime = pkgs.stdenv.mkDerivation {
    name = "ace-app-runtime";
    src = src;
    dontBuild = true;
    installPhase = ''
      mkdir -p $out/app
      cp -r . $out/app
    '';
  };

  # acePackage: the actual thing nix-container-lib pulls in via
  # Lib.flakePackage "aceApp" "default". Its $out/bin/ace-viz and
  # $out/bin/polar-healthcheck are what container-ace.dhall's
  # `entrypoint`/`polar-healthcheck` layer expect to find on PATH inside
  # the built container.
  #
  # Both wrapper scripts reference ${venvNoCheck} and ${appRuntime} as
  # real Nix store paths (substituted at build time via the ${...}
  # string interpolation below) -- there is no /app inside a
  # nix-container-lib image the way there was under our old
  # dockerTools.buildImage + copyToRoot approach, so a hardcoded /app/...
  # path would silently fail at runtime.
  acePackage = pkgs.stdenv.mkDerivation {
    name = "ace-viz";
    dontUnpack = true;
    installPhase = ''
      mkdir -p $out/bin
      cat > $out/bin/ace-viz <<EOF
      #!/usr/bin/env bash
      set -euo pipefail
      exec ${venvNoCheck}/bin/python ${appRuntime}/app/app.py
      EOF
      chmod +x $out/bin/ace-viz
    '';
  };
in {
  inherit venv venvNoCheck appRuntime acePackage;
}
