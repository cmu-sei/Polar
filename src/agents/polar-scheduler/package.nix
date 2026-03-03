{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:

let
  # Build the processor binary (polar-scheduler)
  processor = craneLib.buildPackage (crateArgs // {
    pname = "polar-scheduler";
    cargoExtraArgs = "--bin polar-scheduler --locked";
    src = workspaceFileset ./processor;
    doCheck = false;        # tests run separately in CI
  });

  # Build the observer binary (polar-scheduler-observer)
  observer = craneLib.buildPackage (crateArgs // {
    pname = "polar-scheduler-observer";
    cargoExtraArgs = "--bin polar-scheduler-observer --locked";
    src = workspaceFileset ./observer;
    doCheck = false;
  });

  # Environment for the processor – includes the binary and common paths (CA certs, etc.)
  processorEnv = pkgs.buildEnv {
    name = "processor-env";
    paths = commonPaths ++ [ processor ];
    pathsToLink = [ "/bin" "/etc/ssl/certs" ];
  };

  # Environment for the observer
  observerEnv = pkgs.buildEnv {
    name = "observer-env";
    paths = commonPaths ++ [ observer ];
    pathsToLink = [ "/bin" "/etc/ssl/certs" ];
  };

  # Layered container image for the processor
  processorImage = pkgs.dockerTools.buildLayeredImage {
    name = "polar-scheduler-processor";
    tag = "latest";
    contents = [ processorEnv ];
    config = {
      User = "${commonUser.uid}:${commonUser.gid}";
      Cmd = [ "polar-scheduler" ];
      WorkingDir = "/";
      Env = [
        "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=/etc/ssl/certs"
        # Runtime environment variables (e.g. GRAPH_ENDPOINT) should be set by the orchestrator
      ];
    };
    maxLayers = 100;        # optional, for better layer sharing
  };

  # Layered container image for the observer
  observerImage = pkgs.dockerTools.buildLayeredImage {
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
    maxLayers = 100;
  };

in {
  inherit processor observer processorImage observerImage;
}
