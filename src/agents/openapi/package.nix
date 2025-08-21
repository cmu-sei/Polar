{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:


let

    observer = craneLib.buildPackage (crateArgs  // {
    pname = "web-observer";
    cargoExtraArgs = "--bin web-observer --locked";
    src = workspaceFileset ./web/observe;
    });
    consumer = craneLib.buildPackage (crateArgs // {
    pname = "web-consumer";
    cargoExtraArgs = "--bin web-consumer --locked";
    src = workspaceFileset ./web/consume;
    });

    observerImage = pkgs.dockerTools.buildImage {
      name = "polar-web-observer";
      tag = "latest";
      copyToRoot = commonPaths ++ [ observer ];
      uid = commonUser.uid;
      gid = commonUser.gid;

      config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "web-observer" ];
        WorkingDir = "/";
        Env = [ ];
      };
    };

    consumerImage = pkgs.dockerTools.buildImage {
      name = "polar-web-consumer";
      tag = "latest";
      copyToRoot = commonPaths ++ [ consumer ];
      uid = commonUser.uid;
      gid = commonUser.gid;

      config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "web-consumer" ];
        WorkingDir = "/";
        Env = [ ];
      };
    };
in
{
  inherit observer consumer observerImage consumerImage;
}
