# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact
# permission@sei.cmu.edu for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670
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
          { attrPath = "rsync"; flakeInput = null; }
        ];
      })
  ];
  pipeline = null;
  shell = u:
    u.Interactive {
      colorScheme = "gruvbox";
      plugins = [];
      shell = "/bin/nu";
      viBindings = true;
    };
  ssh = { enable = false; port = 2223; };
  staticGid = null;
  staticUid = null;
  tls = null;
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
