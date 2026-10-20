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
  ai = null;
  entrypoint = null;
  extraEnv = [
    { name = "GRAPH_DB"; placement = u: u.StartTime; value = "neo4j"; }
    {
      name = "GRAPH_ENDPOINT";
      placement = u:
        u.StartTime;
      value = "bolt+s://localhost:7687";
    }
    { name = "GRAPH_USER"; placement = u: u.StartTime; value = "neo4j"; }
    {
      name = "POLAR_HEALTH_FILE";
      placement = u:
        u.StartTime;
      value = "/tmp/polar-health/polar-health.json";
    }
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
      u.Custom {
        name = "python-dev";
        packages = [ { attrPath = "default"; flakeInput = "ace-python-env"; } ];
      })
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
          { attrPath = "skopeo"; flakeInput = null; }
          { attrPath = "kubectl"; flakeInput = null; }
          { attrPath = "bash"; flakeInput = null; }
          { attrPath = "diffutils"; flakeInput = null; }
          { attrPath = "default"; flakeInput = "polar-tools"; }
          { attrPath = "default"; flakeInput = "polar-dhall-cache"; }
          { attrPath = "git"; flakeInput = null; }
          { attrPath = "curl"; flakeInput = null; }
          { attrPath = "dhall"; flakeInput = null; }
          { attrPath = "dhall-json"; flakeInput = null; }
          { attrPath = "dhall-nix"; flakeInput = null; }
          { attrPath = "dropbear"; flakeInput = null; }
          { attrPath = "mermaid-cli"; flakeInput = null; }
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
  ssh = { enable = true; port = 2223; };
  staticGid = null;
  staticUid = null;
  tls = null;
  user = {
    createUser = true;
    defaultShell = "/bin/fish";
    skeletonPath = "/etc/container-skel";
    supplementalGroups = [];
  };
}
