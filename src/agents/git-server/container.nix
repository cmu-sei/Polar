{
  ai = null;
  entrypoint = "git-server-entrypoint";
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
    { name = "GIT_HTTP_EXPORT_ALL"; placement = u: u.BuildTime; value = "1"; }
    {
      name = "GIT_PROJECT_ROOT";
      placement = u:
        u.BuildTime;
      value = "/srv/git";
    }
  ];
  mode = u:
    u.Minimal;
  name = "polar-git-server";
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
        name = "git-server-deps";
        packages = [
          { attrPath = "cacert"; flakeInput = null; }
          { attrPath = "git"; flakeInput = null; }
          { attrPath = "nginx"; flakeInput = null; }
          { attrPath = "fcgiwrap"; flakeInput = null; }
        ];
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
