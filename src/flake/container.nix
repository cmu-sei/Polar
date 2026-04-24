{
  ai = null;
  entrypoint = null;
  extraEnv = [
    { name = "GRAPH_DB"; placement = u: u.BuildTime; value = "neo4j"; }
    {
      name = "GRAPH_ENDPOINT";
      placement = u:
        u.BuildTime;
      value = "bolt://127.0.0.1:7687";
    }
    { name = "GRAPH_USER"; placement = u: u.BuildTime; value = "neo4j"; }
  ];
  mode = u:
    u.Dev;
  name = "polar-dev";
  nix = {
    buildUserCount = u:
      u.Dynamic;
    enableDaemon = true;
    sandboxPolicy = u:
      u.Auto;
    trustedUsers = [ "root" ];
  };
  packageLayers = [
    (u:
      u.Micro)
    (u:
      u.Core)
    (u:
      u.InteractiveDev)
    (u:
      u.RustToolchain)
    (u:
      u.Custom {
        name = "polar-extras";
        packages = [
          { attrPath = "default"; flakeInput = "staticanalysis"; }
          { attrPath = "default"; flakeInput = "dotacat"; }
          { attrPath = "default"; flakeInput = "myNeovimOverlay"; }
          { attrPath = "sops"; flakeInput = null; }
          { attrPath = "oras"; flakeInput = null; }
          { attrPath = "zed-editor"; flakeInput = null; }
          { attrPath = "rage"; flakeInput = null; }
          { attrPath = "cosign"; flakeInput = null; }
          { attrPath = "default"; flakeInput = "cassini-client"; }
          { attrPath = "git"; flakeInput = null; }
          { attrPath = "curl"; flakeInput = null; }
          { attrPath = "dhall"; flakeInput = null; }
          { attrPath = "dhall-json"; flakeInput = null; }
          { attrPath = "dhall-nix"; flakeInput = null; }
        ];
      })
  ];
  pipeline = null;
  shell = u:
    u.Interactive {
      colorScheme = "gruvbox";
      plugins = [ "bobthefish" "bass" "grc" ];
      shell = "/bin/fish";
      viBindings = true;
    };
  ssh = { enable = false; port = 2223; };
  staticGid = null;
  staticUid = null;
  tls = { certsPath = null; enable = true; generateCerts = true; };
  user = {
    createUser = true;
    defaultShell = "/bin/fish";
    skeletonPath = "/etc/container-skel";
    supplementalGroups = [];
  };
}
