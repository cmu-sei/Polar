{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
}:

let
  polarInitBin = craneLib.buildPackage (crateArgs // {
    pname = "polar-init";
    cargoExtraArgs = "--bin polar-init --locked";
    src = workspaceFileset ./.;
    doCheck = false;
  });

  polarInitContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ polarInitBin ];
  };

in
{
  inherit polarInitBin;
  image = polarInitContainer.image;
}
