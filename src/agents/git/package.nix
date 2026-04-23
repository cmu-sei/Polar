# src/agents/git/package.nix
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
    pname = "git-repo-observer";
    cargoExtraArgs = "--bin git-repo-observer --locked";
    src = workspaceFileset ./git/observe;
  });

  consumer = craneLib.buildPackage (crateArgs // {
    pname = "git-repo-processor";
    cargoExtraArgs = "--bin git-repo-processor --locked";
    src = workspaceFileset ./git/consume;
  });

  scheduler = craneLib.buildPackage (crateArgs // {
    pname = "scheduler";
    cargoExtraArgs = "--bin scheduler --locked";
    src = workspaceFileset ./git/scheduler;
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

  schedulerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-scheduler.nix;
    extraDerivations = [ scheduler ];
  };
in
{
  inherit observer consumer scheduler;
  observerImage  = observerContainer.image;
  processorImage = consumerContainer.image;
  schedulerImage = schedulerContainer.image;
}
