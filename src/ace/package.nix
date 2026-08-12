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
