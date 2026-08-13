# src/agents/polar-scheduler/package.nix
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

  healthcheckInputs = inputs // {
    polar-healthcheck = {
      packages.${system}.default = healthcheckDrv;
    };
  };

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ processor ];
  };

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-observer.nix;
    extraDerivations = [ observer ];
  };
in
{
  inherit processor observer;
  processorImage = processorContainer.image;
  observerImage  = observerContainer.image;
}
