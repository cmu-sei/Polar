{
  ai = { enable = true; llamaPort = 8080; modelsPath = "/opt/llama-models"; };
  entrypoint = null;
  extraEnv = [
    { name = "LLAMA_HOST"; placement = u: u.BuildTime; value = "0.0.0.0"; }
    { name = "LLAMA_PORT"; placement = u: u.BuildTime; value = "8080"; }
    { name = "LLAMA_CTX_SIZE"; placement = u: u.BuildTime; value = "32768"; }
    { name = "LLAMA_GPU_LAYERS"; placement = u: u.BuildTime; value = "99"; }
    {
      name = "LLAMA_BASE_URL";
      placement = u:
        u.BuildTime;
      value = "http://localhost:8080/v1";
    }
    {
      name = "OLLAMA_HOST";
      placement = u:
        u.StartTime;
      value = "0.0.0.0:8080";
    }
    { name = "ANTHROPIC_API_KEY"; placement = u: u.UserProvided; value = ""; }
    { name = "OPENAI_API_KEY"; placement = u: u.UserProvided; value = ""; }
    { name = "OPENROUTER_API_KEY"; placement = u: u.UserProvided; value = ""; }
  ];
  mode = u:
    u.AIAgent;
  name = "polar-agent";
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
      u.Core)
    (u:
      u.RustToolchain)
    (u:
      u.Custom {
        name = "polar-agent-tools";
        packages = [
          { attrPath = "default"; flakeInput = "llamaCpp"; }
          { attrPath = "just"; flakeInput = null; }
          { attrPath = "curl"; flakeInput = null; }
        ];
      })
  ];
  pipeline = null;
  shell = u:
    u.Minimal { shell = "/bin/nu"; };
  ssh = { enable = false; port = 2223; };
  staticGid = null;
  staticUid = null;
  tls = { certsPath = null; enable = true; generateCerts = true; };
  user = {
    createUser = true;
    defaultShell = "/bin/fish";
    skeletonPath = "/etc/container-skel";
    supplementalGroups = [
      { gid = 44; name = "video"; }
      { gid = 110; name = "render"; }
    ];
  };
}
