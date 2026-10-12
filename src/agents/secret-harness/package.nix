# src/agents/secret-harness/package.nix
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
  harness = craneLib.buildPackage (crateArgs // {
    pname = "secret-harness";
    cargoExtraArgs = "--bin secret-harness --locked";
    src = workspaceFileset ./.;
  });

  container = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath = ./container.nix;
    extraDerivations = [ harness ];
  };
in
{
  inherit harness;
  image = container.image;
}
