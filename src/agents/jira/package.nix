# src/agents/jira/package.nix
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
    pname = "jira-observer";
    cargoExtraArgs = "--bin jira-observer --locked";
    src = workspaceFileset ./jira/observe;
  });

  processor = craneLib.buildPackage (crateArgs // {
    pname = "jira-processor";
    cargoExtraArgs = "--bin jira-processor --locked";
    src = workspaceFileset ./jira/process;
  });

  observerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-observer.nix;
    extraDerivations = [ observer ];
  };

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ processor ];
  };
in
{
  inherit observer processor;
  observerImage  = observerContainer.image;
  processorImage = processorContainer.image;
}
