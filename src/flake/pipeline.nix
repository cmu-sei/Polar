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
  artifactDir = "/workspace/pipeline-out";
  name = "polar-devsecops";
  outputs = null;
  stages = [
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
      command = "nu ../../scripts/nushell/cargo-sbom.nu";
      condition = null;
      failureMode = u:
        u.Collect;
      impurityReason = null;
      inputs = [ (u: u.Workspace) ];
      name = "make-sboms";
      outputs = [
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "build-orchestrator";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "build-processor";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "build.exit";
          })
        (u:
          u.Artifact { "content_type" = "elf-binary-set"; name = "build.log"; })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "cassini-c2";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "cassini-client";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "cassini-server";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "config-ops";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "event-logger";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "git-repo-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "git-repo-processor";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "gitlab-consumer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "gitlab-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "harness-controller";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "harness-producer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "harness-sink";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "jira-consumer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "jira-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "kube-consumer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "kube-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "mock-adhoc-agent";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "mock-agent";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "openapi-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "orchestrator-core";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "pipeline-manifest.pinned.json";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "polar-scheduler";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "polar-scheduler-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "policy-config";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "provenance-linker";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "provenance-resolver";
          })
        (u:
          u.Artifact { "content_type" = "elf-binary-set"; name = "scheduler"; })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "web-consumer";
          })
      ];
      pure = true;
    }
    {
      command = "nu ../../scripts/cargo-attested-build.nu";
      condition = null;
      failureMode = u:
        u.Collect;
      impurityReason = null;
      inputs = [ (u: u.Workspace) ];
      name = "attested-build";
      outputs = [
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "build-orchestrator";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "build-processor";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "build.exit";
          })
        (u:
          u.Artifact { "content_type" = "elf-binary-set"; name = "build.log"; })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "cassini-c2";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "cassini-client";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "cassini-server";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "config-ops";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "event-logger";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "git-repo-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "git-repo-processor";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "gitlab-consumer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "gitlab-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "harness-controller";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "harness-producer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "harness-sink";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "jira-consumer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "jira-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "kube-consumer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "kube-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "mock-adhoc-agent";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "mock-agent";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "openapi-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "orchestrator-core";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "pipeline-manifest.pinned.json";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "polar-scheduler";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "polar-scheduler-observer";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "policy-config";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "provenance-linker";
          })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "provenance-resolver";
          })
        (u:
          u.Artifact { "content_type" = "elf-binary-set"; name = "scheduler"; })
        (u:
          u.Artifact {
            "content_type" = "elf-binary-set";
            name = "web-consumer";
          })
      ];
      pure = true;
    }
  ];
  workingDir = "/workspace/src/agents";
}
