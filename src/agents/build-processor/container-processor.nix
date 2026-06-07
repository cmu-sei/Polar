{
  ai = null;
  entrypoint = "build-processor";
  extraEnv = [];
  mode = u:
    u.Minimal;
  name = "build-processor";
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
