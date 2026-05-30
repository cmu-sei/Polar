{
  ai = null;
  entrypoint = "cert-issuer";
  extraEnv = [
    { name = "RUST_LOG"; placement = u: u.BuildTime; value = "debug"; }
  ];
  mode = u:
    u.Minimal;
  name = "polar-cert-issuer";
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
