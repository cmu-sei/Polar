# src/agents/polar-scheduler/package.nix
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

  processor = craneLib.buildPackage (crateArgs // {
    pname = "polar-scheduler";
    cargoExtraArgs = "--bin polar-scheduler --locked";
    src = workspaceFileset ./processor;
    doCheck = false;
  });

  observer = craneLib.buildPackage (crateArgs // {
    pname = "polar-scheduler-observer";
    cargoExtraArgs = "--bin polar-scheduler-observer --locked";
    src = workspaceFileset ./observer;
    doCheck = false;
  });

  processorEnv = pkgs.buildEnv {
    name = "processor-env";
    paths = commonPaths ++ certPaths ++ [ processor ];
    pathsToLink = [ "/bin" "/etc/ssl/certs" ];
  };

  observerEnv = pkgs.buildEnv {
    name = "observer-env";
    paths = commonPaths ++ certPaths ++ [ observer ];
    pathsToLink = [ "/bin" "/etc/ssl/certs" ];
  };

  processorImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "polar-scheduler-processor";
    tag = "latest";
    contents = [ processorEnv ];
    maxLayers = 20;
    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "polar-scheduler" ];
      WorkingDir = "/";
      Env = [
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=/etc/ssl/certs"
      ];
    };
  };

  observerImage = pkgs.dockerTools.buildLayeredImage {
    inherit extraCommands;
    name = "polar-scheduler-observer";
    tag = "latest";
    contents = [ observerEnv ];
    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "polar-scheduler-observer" ];
      WorkingDir = "/";
      Env = [
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=/etc/ssl/certs"
      ];
    };
    maxLayers = 20;
  };

in {
  inherit processor observer processorImage observerImage;
}
