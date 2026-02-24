{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:


let

  observerConfig = pkgs.writeTextFile {
    name          = "git.json";
    destination   = "/git.json";
    text          = builtins.readFile ./observer/conf/git.json;
  };

  observer = craneLib.buildPackage (crateArgs  // {
  pname = "git-repo-observer";
  cargoExtraArgs = "--bin git-repo-observer --locked";
  src = workspaceFileset ./git/observe;
  });


  consumer = craneLib.buildPackage (crateArgs // {
  pname = "git-consumer";
  cargoExtraArgs = "--bin git-repo-processor --locked";
  src = workspaceFileset ./git/consume;
  });

  scheduler = craneLib.buildPackage (crateArgs  // {
  pname = "scheduler";
  cargoExtraArgs = "--bin scheduler --locked";
  src = workspaceFileset ./git/scheduler;
  });



  observerImage = pkgs.dockerTools.buildImage {
    name = "polar-git-repo-observer";
    tag = "latest";
    copyToRoot = commonPaths ++ [ observer observerConfig ];
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "git-repo-observer" ];
      WorkingDir = "/";
      Env = [ ];
    };
  };

  consumerImage = pkgs.dockerTools.buildImage {
    name = "polar-git-consumer";
    tag = "latest";
    copyToRoot = commonPaths ++ [ consumer ];
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "git-consumer" ];
      WorkingDir = "/";
      Env = [ ];
    };
  };

  schedulerImage = pkgs.dockerTools.buildImage {
    name = "polar-git-scheduler";
    tag = "latest";
    copyToRoot = commonPaths ++ [ scheduler ];
    uid = commonUser.uid;
    gid = commonUser.gid;

    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "scheduler" ];
      WorkingDir = "/";
      Env = [ ];
    };
  };
in
{
  inherit observer consumer observerImage consumerImage scheduler schedulerImage;
}
