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
  linkerBin = craneLib.buildPackage (crateArgs // {
    pname = "provenance-linker";
    cargoExtraArgs = "--bin provenance-linker --locked";
    src = workspaceFileset ./provenance;
  });

  resolverBin = craneLib.buildPackage (crateArgs // {
    pname = "provenance-resolver";
    cargoExtraArgs = "--bin provenance-resolver --locked";
    src = workspaceFileset ./provenance;
  });

  resolverConfig = pkgs.writeTextFile {
    name        = "resolver.json";
    destination = "/resolver.json";
    text        = builtins.readFile ./resolver/resolver.json;
  };

  linkerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-linker.nix;
    extraDerivations = [ linkerBin ];
  };

  resolverContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-resolver.nix;
    extraDerivations = [ resolverBin resolverConfig ];
  };
in
{
  inherit linkerBin resolverBin;
  linkerImage   = linkerContainer.image;
  resolverImage = resolverContainer.image;
}
