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
    pname = "web-observer";
    cargoExtraArgs = "--bin web-observer --locked";
    src = workspaceFileset ./openapi/observe;
    });
    consumer = craneLib.buildPackage (crateArgs // {
    pname = "web-consumer";
    cargoExtraArgs = "--bin web-consumer --locked";
    src = workspaceFileset ./openapi/consume;
    });

    observerImage = pkgs.dockerTools.buildImage {
      inherit extraCommands;
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
      inherit extraCommands;
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
