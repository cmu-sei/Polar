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
  resolverConfig = pkgs.writeTextFile {
    name        = "resolver.json";
    destination = "/resolver.json";
    text        = builtins.readFile ./resolver.json;
  };


  resolverContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-resolver.nix;
    extraDerivations = [ resolverBin resolverConfig ];
  };
in
{
  inherit resolverBin;
  resolverImage = resolverContainer.image;
}
