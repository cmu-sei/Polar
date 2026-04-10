{
  ai = null;
  entrypoint = "nu-init-entrypoint";
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
  name = "polar-nu-init";
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
      u.Core)
    (u:
      u.Custom {
        name = "nu-init-deps";
        packages = [ { attrPath = "cacert"; flakeInput = null; } ];
      })
  ];
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
