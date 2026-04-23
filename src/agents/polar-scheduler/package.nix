# src/agents/polar-scheduler/package.nix
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
  processor = craneLib.buildPackage (crateArgs // {
    pname = "polar-scheduler";
    cargoExtraArgs = "--bin polar-scheduler --locked";
    src = workspaceFileset ./polar-scheduler/processor;
    doCheck = false;
  });

  observer = craneLib.buildPackage (crateArgs // {
    pname = "polar-scheduler-observer";
    cargoExtraArgs = "--bin polar-scheduler-observer --locked";
    src = workspaceFileset ./polar-scheduler/observer;
    doCheck = false;
  });

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ processor ];
  };

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-observer.nix;
    extraDerivations = [ observer ];
  };
in
{
  inherit processor observer;
  processorImage = processorContainer.image;
  observerImage  = observerContainer.image;
}
