# src/agents/provenance/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, ...
}:
let

  resolverBin = craneLib.buildPackage (crateArgs // {
    pname = "oci-resolver";
    cargoExtraArgs = "--bin oci-resolver --locked";
    src = workspaceFileset ./.;
  });

  resolverContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-resolver.nix;
    extraDerivations = [ resolverBin ];
  };
in
{
  inherit resolverBin;
  resolverImage = resolverContainer.image;
}
