{
  ai = null;
  entrypoint = "polar-analyze-cargo";
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
  ];
  mode = u:
    u.Minimal;
  name = "polar-analyzer-cargo-manifest";
  nix = {
    buildUserCount = u:
      u.Dynamic;
    enableDaemon = false;
    sandboxPolicy = u:
      u.Auto;
    trustedUsers = [ "root" ];
  };
  packageLayers = [ (u: u.Micro) ];
  pipeline = null;
  shell = null;
  ssh = null;
  staticGid = 65532;
  staticUid = 65532;
  tls = null;
  user = {
    createUser = false;
    defaultShell = "/bin/fish";
    skeletonPath = "/etc/container-skel";
    supplementalGroups = [];
  };
}
