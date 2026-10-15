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
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

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
