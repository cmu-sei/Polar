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
