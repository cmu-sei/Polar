{
  ai = null;
  entrypoint = "kube-observer";
  extraEnv = [
    {
      name = "SSL_CERT_FILE";
      placement = u:
        u.BuildTime;
      value = "/etc/ssl/certs/ca-bundle.crt";
    }
    {
      name = "SSL_CERT_DIR";
      placement = u:
        u.BuildTime;
      value = "/etc/ssl/certs";
    }
    {
      name = "POLAR_HEALTH_CERTS";
      placement = u:
        u.BuildTime;
      value = "/etc/tls/certs/cert.pem";
    }
  ];
  mode = u:
    u.Minimal;
  name = "kube-observer";
  nix = {
    buildUserCount = u:
      u.Dynamic;
    enableDaemon = false;
    sandboxPolicy = u:
      u.Auto;
    trustedUsers = [ "root" ];
  };
  packageLayers = [
    (u:
      u.Micro)
    (u:
      u.Custom {
        name = "polar-healthcheck-bin";
        packages = [
          { attrPath = "default"; flakeInput = "polar-healthcheck"; }
        ];
      })
  ];
  pipeline = null;
  shell = null;
  ssh = null;
  staticGid = 1000;
  staticUid = 1000;
  tls = null;
  user = {
    createUser = false;
    defaultShell = "/bin/fish";
    skeletonPath = "/etc/container-skel";
    supplementalGroups = [];
  };
}
