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
    pname = "gitlab-observer";
    cargoExtraArgs = "--bin gitlab-observer --locked";
    src = workspaceFileset ./gitlab/observe;
    });
    consumer = craneLib.buildPackage (crateArgs // {
    pname = "gitlab-consumer";
    cargoExtraArgs = "---bin gitlab-consumer --locked";
    src = workspaceFileset ./gitlab/consume;
    });



    observerImage = pkgs.dockerTools.buildImage {
      name = "polar-gitlab-observer";
      tag = "latest";
      copyToRoot = commonPaths ++ [ observer ];
      uid = commonUser.uid;
      gid = commonUser.gid;

      config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "gitlab-observer" ];
        WorkingDir = "/";
        Env = [ ];
      };
    };

    consumerImage = pkgs.dockerTools.buildImage {
      name = "polar-gitlab-consumer";
      tag = "latest";
      copyToRoot = commonPaths ++ [ consumer ];
      uid = commonUser.uid;
      gid = commonUser.gid;

      config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "gitlab-consumer" ];
        WorkingDir = "/";
        Env = [ ];
      };
    };
in
{
  inherit observer consumer observerImage consumerImage;
}
