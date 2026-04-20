# src/agents/git/package.nix
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
    mkdir -p etc tmp
    chmod 1777 tmp
    printf 'polar:x:1000:1000::/home/polar:/bin/bash\n' > etc/passwd
    printf 'polar:x:1000:\n' > etc/group
    printf 'polar:!x:::::::\n' > etc/shadow
  '';

  certPaths = [ pkgs.cacert ];

  observer = craneLib.buildPackage (crateArgs // {
    pname = "git-repo-observer";
    cargoExtraArgs = "--bin git-repo-observer --locked";
    src = workspaceFileset ./git/observe;
  });
  consumer = craneLib.buildPackage (crateArgs // {
    pname = "git-consumer";
    cargoExtraArgs = "--bin git-repo-processor --locked";
    src = workspaceFileset ./git/consume;
  });
  scheduler = craneLib.buildPackage (crateArgs // {
    pname = "scheduler";
    cargoExtraArgs = "--bin scheduler --locked";
    src = workspaceFileset ./git/scheduler;
  });

  observerEnv = pkgs.buildEnv {
    name = "observer-env";
    paths = commonPaths ++ certPaths ++ [ observer ];
    pathsToLink = [ "/bin" "/etc/ssl/certs" ];
  };
  consumerEnv = pkgs.buildEnv {
    name = "consumer-env";
    paths = commonPaths ++ certPaths ++ [ consumer ];
    pathsToLink = [ "/bin" "/etc/ssl/certs" ];
  };
  schedulerEnv = pkgs.buildEnv {
    name = "scheduler-env";
    paths = commonPaths ++ certPaths ++ [ scheduler ];
    pathsToLink = [ "/bin" "/etc/ssl/certs" ];
  };

  observerImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "polar-git-repo-observer";
    tag = "latest";
    contents = [ observerEnv ];
    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "git-repo-observer" ];
      WorkingDir = "/";
      Env = [
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=/etc/ssl/certs"
      ];
    };
    maxLayers = 20;
  };
  consumerImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "polar-git-consumer";
    tag = "latest";
    contents = [ consumerEnv ];
    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "git-repo-processor" ];
      WorkingDir = "/";
      Env = [
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=/etc/ssl/certs"
      ];
    };
    maxLayers = 20;
  };
  schedulerImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "polar-git-scheduler";
    tag = "latest";
    contents = [ schedulerEnv ];
    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "scheduler" ];
      WorkingDir = "/";
      Env = [
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=/etc/ssl/certs"
      ];
    };
    maxLayers = 20;
  };
in
{
  inherit observer consumer observerImage consumerImage scheduler schedulerImage;
}
