# src/agents/kubernetes/package.nix
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
    pname = "kube-observer";
    cargoExtraArgs = "--bin kube-observer --locked";
    src = workspaceFileset ./kubernetes/observe;
  });

  consumer = craneLib.buildPackage (crateArgs // {
    pname = "kube-consumer";
    cargoExtraArgs = "--bin kube-consumer --locked";
    src = workspaceFileset ./kubernetes/consume;
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
