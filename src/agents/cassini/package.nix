# src/agents/cassini/package.nix
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
  cassini = craneLib.buildPackage (crateArgs // {
    pname = "cassini-server";
    cargoExtraArgs = "--bin cassini-server --locked";
    src = workspaceFileset ./broker;
    doCheck = false;
  });

  client = craneLib.buildPackage (crateArgs // {
    pname = "cassini-client";
    cargoExtraArgs = "--bin cassini-client --locked";
    src = workspaceFileset ./client;
    doCheck = false;
  });

  harnessProducer = craneLib.buildPackage (crateArgs // {
    pname = "harness-producer";
    cargoExtraArgs = "--bin harness-producer --locked";
    src = workspaceFileset ./test/producer;
    doCheck = false;
  });

  harnessSink = craneLib.buildPackage (crateArgs // {
    pname = "harness-sink";
    cargoExtraArgs = "--bin harness-sink --locked";
    src = workspaceFileset ./test/sink;
    doCheck = false;
  });

  cassiniContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-cassini.nix;
    extraDerivations = [ cassini ];
  };

  producerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-harness-producer.nix;
    extraDerivations = [ harnessProducer ];
  };

  sinkContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-harness-sink.nix;
    extraDerivations = [ harnessSink ];
  };
in
{
  inherit cassini client harnessProducer harnessSink;
  cassiniImage  = cassiniContainer.image;
  producerImage = producerContainer.image;
  sinkImage     = sinkContainer.image;
}
