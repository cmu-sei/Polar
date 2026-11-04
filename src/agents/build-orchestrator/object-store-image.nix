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

# An S3-compatible object store for build-orchestrator's logs, for a cluster that has none: the Versity S3 gateway over a
# directory, from nixpkgs. (MinIO's community edition is no longer maintained and nixpkgs marks it insecure.)
# On a cloud, a bucket takes its place and this is not deployed.
#
# The gateway wants its directories to exist, and a new volume has none, so the entrypoint makes them. Its
# arguments are the deployment's (library `types/build-orchestrator.dhall`), and its keys are the variables
# ROOT_ACCESS_KEY and ROOT_SECRET_KEY.
{ pkgs }:
let
  entrypoint = pkgs.writeShellApplication {
    name = "build-orchestrator-object-store";
    runtimeInputs = [ pkgs.coreutils pkgs.versitygw ];
    text = ''
      mkdir -p /data/buckets /data/sidecar
      exec versitygw "$@"
    '';
  };
in
pkgs.dockerTools.buildLayeredImage {
  name = "build-orchestrator-object-store";
  tag = "latest";
  contents = [ entrypoint pkgs.cacert ];
  config = {
    Entrypoint = [ "/bin/build-orchestrator-object-store" ];
    Env = [ "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" "PATH=/bin" ];
    ExposedPorts = { "9000/tcp" = { }; };
  };
}
