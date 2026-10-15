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
  entrypoint = "kube-observer";
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
    {
      name = "POLAR_HEALTH_CERTS";
      placement = u:
        u.BuildTime;
      value = "/etc/tls/certs/cert.pem";
    }
  ];
  mode = u:
    u.Minimal;
  name = "kube-observer";
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
      u.Custom {
        name = "polar-healthcheck-bin";
        packages = [
          { attrPath = "default"; flakeInput = "polar-healthcheck"; }
        ];
      })
  ];
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
