# src/agents/gitlab/package.nix
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
  observer = craneLib.buildPackage (crateArgs // {
    pname = "gitlab-observer";
    cargoExtraArgs = "--bin gitlab-observer --locked";
    src = workspaceFileset ./gitlab/observe;
  });

  consumer = craneLib.buildPackage (crateArgs // {
    pname = "gitlab-consumer";
    cargoExtraArgs = "--bin gitlab-consumer --locked";
    src = workspaceFileset ./gitlab/consume;
  });

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-observer.nix;
    extraDerivations = [ observer ];
  };

  consumerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-consumer.nix;
    extraDerivations = [ consumer ];
  };
in
{
  inherit observer consumer;
  observerImage = observerContainer.image;
  consumerImage = consumerContainer.image;
}
