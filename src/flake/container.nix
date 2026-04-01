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
      u.Core)
    (u:
      u.CI)
    (u:
      u.Dev)
    (u:
      u.Toolchain)
    (u:
      u.Pipeline)
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
        ];
      })
  ];
  pipeline = {
    artifactDir = "/workspace/pipeline-out";
    name = "polar-devsecops";
    outputs = null;
    stages = [
      {
        command = "cargo fmt --check";
        condition = null;
        failureMode = u:
          u.Collect;
        impurityReason = null;
        inputs = [ (u: u.Workspace) ];
        name = "fmt";
        outputs = [ (u: u.None) ];
        pure = true;
      }
      {
        command = "cargo clippy -- -D warnings";
        condition = null;
        failureMode = u:
          u.Collect;
        impurityReason = null;
        inputs = [ (u: u.Workspace) ];
        name = "lint";
        outputs = [ (u: u.None) ];
        pure = true;
      }
      {
        command = "run-analysis --config ./analysis.toml";
        condition = null;
        failureMode = u:
          u.Collect;
        impurityReason = null;
        inputs = [ (u: u.Workspace) ];
        name = "static-analysis";
        outputs = [ (u: u.Report { name = "static-analysis-report"; }) ];
        pure = true;
      }
      {
        command = "run-audit --sbom ./sbom.json";
        condition = null;
        failureMode = u:
          u.Collect;
        impurityReason = null;
        inputs = [ (u: u.Workspace) ];
        name = "audit";
        outputs = [
          (u:
            u.Report { name = "audit-report"; })
          (u:
            u.Artifact { "content_type" = "application/json"; name = "sbom"; })
        ];
        pure = true;
      }
      {
        command = "cargo test --workspace";
        condition = "CI_FULL";
        failureMode = u:
          u.FailFast;
        impurityReason = "Cannot guarantee environment variable is set";
        inputs = [ (u: u.Workspace) ];
        name = "full-test";
        outputs = [ (u: u.None) ];
        pure = false;
      }
    ];
    workingDir = "/workspace/src/agents";
  };
  shell = {
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
