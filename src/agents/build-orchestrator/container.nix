{
  ai = null;
  entrypoint = "git-clone-entrypoint";
  extraEnv = [
    {
      name = "GIT_SSL_CAINFO";
      placement = u:
        u.BuildTime;
      value = "/etc/ssl/certs/ca-bundle.crt";
    }
    { name = "GIT_TERMINAL_PROMPT"; placement = u: u.BuildTime; value = "0"; }
  ];
  mode = u:
    u.Minimal;
  name = "cyclops/git-clone";
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
        name = "git-clone-deps";
        packages = [
          { attrPath = "git"; flakeInput = null; }
          { attrPath = "cacert"; flakeInput = null; }
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
