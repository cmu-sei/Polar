# TODO: A nix module that will package and containerize cassini

{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  commonUser,
}:


let

    orchestrator = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin build-orchestrator --locked";
    src = workspaceFileset ./agent;
    # Disable tests for now, We'll run them later with env vars and TlsCerts
    doCheck = false;
    });


    orchestratorEnv = pkgs.buildEnv {
        name = "orchestrator-env";
        paths = commonPaths ++ [
          orchestrator
        ];

        pathsToLink = [
            "/bin"
            "/etc/ssl/certs"
        ];
    };

    orchestratorImage = pkgs.dockerTools.buildLayeredImage {
    name = "build-orchestrator";
    tag = "latest";
    copyToRoot = commonPaths ++ [
      orchestratorEnv
    ];
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "build-orchestrator" ];
        WorkingDir = "/";
        Env = [
        ];
    };
    };


in
{
  inherit orchestrator orchestratorImage;
}
