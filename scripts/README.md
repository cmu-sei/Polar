# Scripts

The scripts in this directory are as follows:
* *dev_stack.sh:* Start up a local, linux-based development stack. If this is
  not your use-case, you will need to follow the instructions in the various
  documentation. If you're comfortable reading source code, this script will
  explain a lot.
* `make-chart.sh` is a script that generates an umbrella helm chart from our dhall configurations.
* `static-tools.sh` is a script that runs various rust static analysis tools on a given crate or workspace.
* `gitlab-ci.sh` is a script meant to envelop most of our CI/CD tasks in one go. See our [DevOps manual](../docs/devops.md) for details
