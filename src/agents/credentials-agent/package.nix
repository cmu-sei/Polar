# src/agents/credentials-agent/package.nix
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
    pname = "credentials-agent-observer";
    cargoExtraArgs = "--bin credentials-agent-observer --locked";
    src = workspaceFileset ./credentials-agent/observe;
  });

  consumer = craneLib.buildPackage (crateArgs // {
    pname = "credentials-agent-consumer";
    cargoExtraArgs = "--bin credentials-agent-consumer --locked";
    src = workspaceFileset ./credentials-agent/consume;
  });

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath    = ./container-observer.nix;
    extraDerivations = [ observer ];
  };

  consumerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath    = ./container-consumer.nix;
    extraDerivations = [ consumer ];
  };
in
{
  inherit observer consumer;
  observerImage = observerContainer.image;
  consumerImage = consumerContainer.image;
}
