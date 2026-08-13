# src/agents/resolver/package.nix
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

  resolverBin = craneLib.buildPackage (crateArgs // {
    pname = "oci-resolver";
    cargoExtraArgs = "--bin oci-resolver --locked";
    src = workspaceFileset ./.;
  });

  healthcheckInputs = inputs // {
    polar-healthcheck = {
      packages.${system}.default = healthcheckDrv;
    };
  };

  resolverContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-resolver.nix;
    extraDerivations = [ resolverBin ];
  };
in
{
  inherit resolverBin;
  resolverImage = resolverContainer.image;
}
