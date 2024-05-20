#!/bin/bash

# Polar (OSS)

# Copyright 2024 Carnegie Mellon University.

# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING
# INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON
# UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS
# TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE
# OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE
# MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND
# WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

# Licensed under a MIT-style license, please see license.txt or contact
# permission@sei.cmu.edu for full terms.

# [DISTRIBUTION STATEMENT A] This material has been approved for public release
# and unlimited distribution.  Please see Copyright notice for non-US
# Government use and distribution.

# This Software includes and/or makes use of Third-Party Software each subject
# to its own license.

set -euo pipefail
# Ensure aliases do not interfere
unalias -a


# ********** Begin global configuration options **********

# Just a gentle reminder that "0" is "success" in Bash and Bash has no actual
# Boolean type, so we use "0" to represent "true", and "-1" to represent
# "false".

# Get the parent process ID (PPID) of the current script
PPID=$(ps -o ppid= -p $$)

# Get the command name of the parent process
PARENT_CMD=$(ps -o comm= -p $PPID)

# ********** End global configuration options **********

# Define configuration through user input
configure_gitlab_env() {
    echo "Configuring the environment template. Please provide the required values."

    # Example:
    # read -p "Enter GitLab API endpoint [https://example.com/api/v4]: " GITLAB_ENDPOINT
    # GITLAB_ENDPOINT=${GITLAB_ENDPOINT:-"https://example.com/api/v4"}

    read -p "Enter GitLab API endpoint [https://gitlab.sandbox.labz.s-box.org/api/v4]: " GITLAB_ENDPOINT
    GITLAB_ENDPOINT=${GITLAB_ENDPOINT:-"https://gitlab.sandbox.labz.s-box.org/api/v4"}

    # TODO: Add more parameter validation for user inputs
    if [[ ! "$GITLAB_ENDPOINT" =~ ^https:// ]]; then
        echo "Invalid URL format for GitLab Endpoint. Please use a valid URL starting with https://"
        exit 1
    fi

    read -p "Enter your GitLab personal access token: " GITLAB_TOKEN

    read -p "Enter GitLab Observer Config path [$PROJECT_ROOT/src/agents/gitlab/observe/conf/observer_config.yaml]: " GITLAB_OBSERVER_CONFIG
    GITLAB_OBSERVER_CONFIG=${GITLAB_OBSERVER_CONFIG:-"$PROJECT_ROOT/src/agents/gitlab/observe/conf/observer_config.yaml"}
}

# Configuration is written to a file and sourced in the execution shell
env_config_exists() {
    local config_file="$PROJECT_ROOT/conf/env_setup.sh"
    echo "Checking that configuration file exists at $config_file"

    if [[ -f "$config_file" ]]; then
        echo 0
    else
        echo "Environment config file doesn't exist. This script is only meant to be"
        echo "run once. Edit the config directly or delete it to run this script again."
        echo "WARNING: Execution has failed and will clean-up any artifacts"
        echo "potentially created during this run."
        exit -1
    fi
}

modify_env_config() {
    local config_file="$PROJECT_ROOT/conf/env_setup.sh"

    echo '# The absolute file path to the observer_config.yaml. One is available in the' >> "$config_file"
    echo '# following dir: ./gitlab_agent/src/observer/src/observer_config.yaml' >> "$config_file"
    COMMAND="export GITLAB_OBSERVER_CONFIG=\"$GITLAB_OBSERVER_CONFIG\""
    eval "$COMMAND"
    echo "$COMMAND" >> "$config_file"

    echo '# The endpoint must end in /api/v4' >> "$config_file"
    COMMAND="export GITLAB_ENDPOINT=\"$GITLAB_ENDPOINT\""
    eval "$COMMAND"
    echo "$COMMAND" >> "$config_file"

    echo '# A Personal Access Token for the instance (Note: The information returned from' >> "$config_file"
    echo '# GitLab will depend on the permissions granted to the token' >> "$config_file"
    echo '# See Gitlabs REST API docs for more information' >> "$config_file"
    echo '# For reference, GitLab tokens use the form, "glpat-xxxxxxxxxxxxxxxxxxxx"' >> "$config_file"
    COMMAND="export GITLAB_TOKEN=\"$GITLAB_TOKEN\""
    eval "$COMMAND"
    echo "$COMMAND" >> "$config_file"
}

main() {
    # Check if the parent command is the expected one
    EXPECTED_CMD="dev_stack.sh"

    if [ "$PARENT_CMD" != "$EXPECTED_CMD" ]; then
        echo "This script was not called by the expected command. It was called by: $PARENT_CMD"
        exit -1
    fi

    env_config_exists
    configure_gitlab_env
    modify_env_config
}

main
