{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:


let
    extraCommands = ''
      mkdir -p etc
      printf 'polar:x:1000:1000::/home/polar:/bin/bash\n' > etc/passwd
      printf 'polar:x:1000:\n' > etc/group
      printf 'polar:!x:::::::\n' > etc/shadow
    '';

    observer = craneLib.buildPackage (crateArgs  // {
    pname = "gitlab-observer";
    cargoExtraArgs = "--bin gitlab-observer --locked";
    src = workspaceFileset ./gitlab/observe;
    });
    consumer = craneLib.buildPackage (crateArgs // {
    pname = "gitlab-consumer";
    cargoExtraArgs = "--bin gitlab-consumer --locked";
    src = workspaceFileset ./gitlab/consume;
    });



    observerImage = pkgs.dockerTools.buildImage {
      inherit extraCommands;
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
      inherit extraCommands;
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
