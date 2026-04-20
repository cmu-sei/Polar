{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  commonUser,
}:


let
    extraCommands = ''
      mkdir -p etc
      printf 'polar:x:1000:1000::/home/polar:/bin/bash\n' > etc/passwd
      printf 'polar:x:1000:\n' > etc/group
      printf 'polar:!x:::::::\n' > etc/shadow
    '';

    cassini = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin cassini-server --locked";
    src = workspaceFileset ./broker;
    # Disable tests for now, We'll run them later with env vars and TlsCerts
    doCheck = false;
    });


    client = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin cassini-client --locked";
    src = workspaceFileset ./client;
    # Disable tests for now, We'll run them later with env vars and TlsCerts
    doCheck = false;
    });

    # build the test harness services
    harnessProducer = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin harness-producer --locked";
    src = workspaceFileset ./test/producer;
    # Disable tests for now, We'll run them later with env vars and TlsCerts
    doCheck = false;
    });

    harnessSink = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin harness-sink --locked";
    src = workspaceFileset ./test/sink;
    # Disable tests for now, We'll run them later with env vars and TlsCerts
    doCheck = false;
    });

    cassiniEnv = pkgs.buildEnv {
        name = "cassini-env";
        paths = commonPaths ++ [
            cassini
        ];

        pathsToLink = [
            "/bin"
            "/etc/ssl/certs"
        ];
    };

    cassiniImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "cassini";
    tag = "latest";
    contents = commonPaths ++ [
        cassiniEnv
    ];
    maxLayers = 20;
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "cassini-server" ];
        WorkingDir = "/";
        Env = [
        "CASSINI_BIND_ADDR=0.0.0.0:8080"
        ];
    };
    };

    producerImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "harness-producer";
    tag = "latest";
    contents = commonPaths ++ [
        harnessProducer
    ];
    maxLayers = 20;
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        # Cmd = [ "harness-producer" ];
        WorkingDir = "/";
        Env = [];
    };
    };

    sinkImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "harness-sink";
    tag = "latest";
    contents = commonPaths ++ [
        harnessProducer
    ];
    maxLayers = 20;
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
  inherit cassini cassiniImage client harnessProducer harnessSink producerImage sinkImage;
}
