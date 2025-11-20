# TODO: A nix module that will package and containerize cassini

{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:


let

    cassini = craneLib.buildPackage (crateArgs // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--bin cassini-server --locked";
    src = workspaceFileset ./broker;
    # Disable tests for now, We'll run them later with env vars and TlsCerts
    doCheck = false;
    });

    # build the test harness services
    harnessProducer = craneLib.buildPackage (crateArgs // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--bin harness-producer --locked";
    src = workspaceFileset ./test/producer;
    # Disable tests for now, We'll run them later with env vars and TlsCerts
    doCheck = false;
    });

    harnessSink = craneLib.buildPackage (crateArgs // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--bin harness-sink --locked";
    src = workspaceFileset ./test/sink;
    # Disable tests for now, We'll run them later with env vars and TlsCerts
    doCheck = false;
    });

    # cassiniEnv = pkgs.buildEnv {
    #     name = "cassini-env";
    #     paths =  [ cassini ];

    #     pathsToLink = [
    #         "/bin"
    #     ];
    # };

    cassiniImage = pkgs.dockerTools.buildImage {
    name = "cassini";
    tag = "latest";
    copyToRoot = commonPaths ++ [
        cassini
    ];
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "cassini-server" ];
        WorkingDir = "/";
        # Vars are always set at container runtime, others have defaults.
        Env = [];
    };
    };

    producerImage = pkgs.dockerTools.buildImage {
    name = "harness-producer";
    tag = "latest";
    copyToRoot = commonPaths ++ [
        harnessProducer
    ];
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        # Cmd = [ "harness-producer" ];
        WorkingDir = "/";
        Env = [];
    };
    };

    sinkImage = pkgs.dockerTools.buildImage {
    name = "harness-sink";
    tag = "latest";
    copyToRoot = commonPaths ++ [
        harnessProducer
    ];
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        # Cmd = [ "harness-sink" ];
        WorkingDir = "/";
        Env = [];
    };
    };
in
{
  inherit cassini cassiniImage harnessProducer harnessSink producerImage sinkImage;
}
