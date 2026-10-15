# src/agents/git/package.nix
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
  observer = craneLib.buildPackage (crateArgs // {
    pname = "git-repo-observer";
    # The harness proposes a repository to the controller, to try the admission rules from inside the cluster.
    cargoExtraArgs = "--bin git-repo-observer --bin git-discovery-harness --locked";
    src = workspaceFileset ./git/observe;
  });

  consumer = craneLib.buildPackage (crateArgs // {
    pname = "git-repo-processor";
    cargoExtraArgs = "--bin git-repo-processor --locked";
    src = workspaceFileset ./git/process;
  });

  controller = craneLib.buildPackage (crateArgs // {
    pname = "git-repo-controller";
    cargoExtraArgs = "--bin git-repo-controller --locked";
    src = workspaceFileset ./git/controller;
  });

  healthcheckInputs = inputs // {
    polar-healthcheck = {
      packages.${system}.default = healthcheckDrv;
    };
  };

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-observer.nix;
    extraDerivations = [ observer ];
  };

  consumerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-consumer.nix;
    extraDerivations = [ consumer ];
  };

  controllerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = healthcheckInputs;
    configNixPath    = ./container-controller.nix;
    extraDerivations = [ controller ];
  };
in
{
  inherit observer consumer controller;
  observerImage  = observerContainer.image;
  processorImage = consumerContainer.image;
  controllerImage = controllerContainer.image;
}
