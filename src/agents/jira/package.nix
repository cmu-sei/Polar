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
    pname = "jira-observer";
    cargoExtraArgs = "--bin jira-observer --locked";
    src = workspaceFileset ./jira/observe;
    });
    consumer = craneLib.buildPackage (crateArgs // {
    pname = "jira-consumer";
    cargoExtraArgs = "--bin jira-consumer --locked";
    src = workspaceFileset ./jira/consume;
    });

    observerImage = pkgs.dockerTools.buildImage {
      name = "polar-jira-observer";
      tag = "latest";
      copyToRoot = commonPaths ++ [ observer ];
      uid = commonUser.uid;
      gid = commonUser.gid;

      config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "jira-observer" ];
        WorkingDir = "/";
        Env = [ ];
      };
    };

    consumerImage = pkgs.dockerTools.buildImage {
      name = "polar-jira-consumer";
      tag = "latest";
      copyToRoot = commonPaths ++ [ consumer ];
      uid = commonUser.uid;
      gid = commonUser.gid;

      config = {
        User = "${commonUser.uid}:${commonUser.gid}";
        Cmd = [ "jira-consumer" ];
        WorkingDir = "/";
        Env = [ ];
      };
    };
in
{
  inherit observer consumer observerImage consumerImage;
}
