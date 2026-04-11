# src/agents/jira/package.nix
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
    pname = "jira-observer";
    cargoExtraArgs = "--bin jira-observer --locked";
    src = workspaceFileset ./jira/observe;
  });
  consumer = craneLib.buildPackage (crateArgs // {
    pname = "jira-consumer";
    cargoExtraArgs = "--bin jira-consumer --locked";
    src = workspaceFileset ./jira/consume;
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

  observerImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "polar-jira-observer";
    tag = "latest";
    contents = [ observerEnv ];
    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "jira-observer" ];
      WorkingDir = "/";
      Env = [
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=/etc/ssl/certs"
      ];
    };
    maxLayers = 100;
  };
  consumerImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "polar-jira-consumer";
    tag = "latest";
    contents = [ consumerEnv ];
    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "jira-consumer" ];
      WorkingDir = "/";
      Env = [
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=/etc/ssl/certs"
      ];
    };
    maxLayers = 100;
  };
in
{
  inherit observer consumer observerImage consumerImage;
}
