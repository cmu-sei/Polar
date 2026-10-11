# src/agents/polar-git-server/package.nix
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
  server = craneLib.buildPackage (crateArgs // {
    pname = "polar-git-server";
    cargoExtraArgs = "--bin polar-git-server --locked";
    src = workspaceFileset ./server;
  });

  container = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath = ./container.nix;
    # The server runs git (upload-pack, receive-pack) for every request.
    extraDerivations = [ server pkgs.git ];
  };
in
{
  inherit server;
  image = container.image;
}
