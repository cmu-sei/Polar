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

# Commands used in the script could be shadowed by function definitions. Ensure
# they are not unsetting any conflicting functions definitions.
clear_shadows() {
    local commands="ls cp chmod chown echo mkdir cp git openssl sudo tee"

    for cmd in $commands; do
        if declare -F "$cmd" > /dev/null; then
            unset -f "$cmd"
        fi
    done
}

# Check for required commands. No need to check for the core utils or posix
# ones.
check_prereqs() {
    local required="git openssl sudo"
    for cmd in $required; do
        if ! command -v "$cmd" &> /dev/null; then
            echo "Error: Required command '$cmd' is not installed." >&2
            exit 1
        fi
    done
}

# Signal handling for cleanup
setup_trap() {
    trap 'echo "Script interrupted."; exit 1' INT
}

get_project_root() {
    # Determine the script's directory and set the default project root to one level up
    local script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local default_project_root="$(dirname "$script_dir")"

    # Prompt user for project root, prefilling the default
    echo "Enter your project root path (default: $default_project_root):"
    read -r PROJECT_ROOT
    PROJECT_ROOT=${PROJECT_ROOT:-$default_project_root}
    
    echo "Project root set to: $PROJECT_ROOT"
    echo "Starting the setup process..."
}

# Define configuration through user input
configure_environment() {
    echo "Configuring the environment template. Please provide the required values."

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

    read -p "Enter Neo4J endpoint [neo4j://neo4j:7687]: " NEO4J_ENDPOINT
    NEO4J_ENDPOINT=${NEO4J_ENDPOINT:-"neo4j://neo4j:7687"}

    read -p "Enter RabbitMQ endpoint [amqps://rabbitmq:5671/%2f?auth_mechanism=external]: " RABBITMQ_ENDPOINT
    RABBITMQ_ENDPOINT=${RABBITMQ_ENDPOINT:-"amqps://rabbitmq:5671/%2f?auth_mechanism=external"}

    read -p "Enter Neo4J username [neo4j]: " NEO4J_USER
    NEO4J_USER=${NEO4J_USER:-"neo4j"}

    read -p "Enter Neo4J password [Password1]: " NEO4J_PASSWORD
    NEO4J_PASSWORD=${NEO4J_PASSWORD:-"Password1"}

    read -p "Enter Neo4J database name [neo4j]: " NEO4J_DB
    NEO4J_DB=${NEO4J_DB:-"neo4j"}

    read -p "Enter TLS client key path [$PROJECT_ROOT/conf/gitlab_compose/ssl/client_rabbitmq.p12]: " TLS_CLIENT_KEY
    TLS_CLIENT_KEY=${TLS_CLIENT_KEY:-"$PROJECT_ROOT/conf/gitlab_compose/ssl/client_rabbitmq.p12"}

    read -p "Enter TLS key password (leave blank if none): " TLS_KEY_PASSWORD

    read -p "Enter TLS CA certificate path [$PROJECT_ROOT/conf/gitlab_compose/ssl/ca_certificate.pem]: " TLS_CA_CERT
    TLS_CA_CERT=${TLS_CA_CERT:-"$PROJECT_ROOT/conf/gitlab_compose/ssl/ca_certificate.pem"}

    read -p "Enter GitLab Observer Config path [$PROJECT_ROOT/src/agents/gitlab/observe/src/observer_config.yaml]: " GITLAB_OBSERVER_CONFIG
    GITLAB_OBSERVER_CONFIG=${GITLAB_OBSERVER_CONFIG:-"$PROJECT_ROOT/src/agents/gitlab/observe/src/observer_config.yaml"}
}

# Configuration is written to a file and sourced in the execution shell
create_env_config_file() {
    local config_file="$PROJECT_ROOT/conf/env_setup.sh"
    echo "Creating configuration file at $config_file"

    cat <<EOF >"$config_file"
# Generated Environment Configuration. If you edit this, do not re-run setup
# script without backing up this file, first.

# The absolute file path to the observer_config.yaml. One is available in the following dir: ./gitlab_agent/src/observer/src/observer_config.yaml
#export GITLAB_OBSERVER_CONFIG="/home/djshepard/Documents/projects/polar/src/agents/gitlab/observe/src/observer_config.yaml"
# The endpoint must end in /api/v4
export GITLAB_ENDPOINT="$GITLAB_ENDPOINT"

# A Personal Access Token for the instance (Note: The information returned from 
# GitLab will depend on the permissions granted to the token.
# See Gitlab's REST API docs for more information)
# For reference, GitLab tokens use the form, "glpat-xxxxxxxxxxxxxxxxxxxx"
export GITLAB_TOKEN="$GITLAB_TOKEN"

# The service endpoint of the given neo4j instance.
# For local development, this could be "neo4j://neo4j:7687"
export NEO4J_ENDPOINT="$NEO4J_ENDPOINT"

# The service endpoint of the rabbitmq instance. Should be prefixed in amqp://
# For the development container, this could be "rabbitmq". The auth mechanism,
# if specified, is according to your configuration and follows the docs for
# rabbitmq.
# For reference: "amqps://rabbitmq:5671/%2f?auth_mechanism=external"
export RABBITMQ_ENDPOINT="$RABBITMQ_ENDPOINT"

# For the development container, this should be "neo4j"
export NEO4J_USER="$NEO4J_USER"

# For the development container, this will be whatever you set it to be when you set up neo4j.
export NEO4J_PASSWORD="$NEO4J_PASSWORD"

# For the development container, this should be "neo4j"
export NEO4J_DB="$NEO4J_DB"

# The absolute file path to the client .p12 file. This is used by the Rust binaries to auth with RabbitMQ via TLS.
export TLS_CLIENT_KEY="$TLS_CLIENT_KEY"

# If a password was set for the .p12 file, put it here.
export TLS_KEY_PASSWORD="$TLS_KEY_PASSWORD"

# The absolute file path to the ca_certificates.pem file created by TLS_GEN. Used by the Rust binaries to auth with RabbitMQ via TLS.
export TLS_CA_CERT="$TLS_CA_CERT"
export GITLAB_OBSERVER_CONFIG="$GITLAB_OBSERVER_CONFIG"
EOF

    chmod 600 "$config_file"
    source "$config_file"
}

generate_rabbit_certs() {
    # Clone tls-gen, generate SSL files, and set up Neo4J volumes
    echo "Generating SSL certificates using tls-gen..."
    git clone https://github.com/rabbitmq/tls-gen.git
    cd tls-gen/basic
    make CN=rabbitmq

    # Ensure the SSL directory exists and move the generated certificates there
    echo "Moving generated SSL files to the designated directory..."
    local ssl_dir="$PROJECT_ROOT/conf/gitlab_compose/ssl"
    mkdir -p "$ssl_dir"
    cp results/* "$ssl_dir"
    cd "$ssl_dir"
    cp client_rabbitmq.p12 client_rabbitmq.p12.original

# Convert the client .p12 file using OpenSSL
    echo "Converting client p12 file to legacy format using OpenSSL..."
    openssl pkcs12 -legacy -export -inkey client_rabbitmq_key.pem -in client_rabbitmq_certificate.pem -out client_rabbitmq.p12 -passout pass:""

# Adjust permissions for security
    echo "Adjusting permissions for SSL files..."
    sudo chown 1001:root *
    sudo chmod 400 *
}

configure_neo4j() {
    # Provisioning Neo4J directories and setting permissions
    echo "Setting up Neo4J directories and copying configuration files..."

    local neo4j_vol_dir="$PROJECT_ROOT/conf/gitlab_compose/neo4j_volumes"

    mkdir -p "$neo4j_vol_dir/{conf,data,import,logs,plugins}"

    cp -r "$PROJECT_ROOT/conf/neo4j_setup/plugins/"* "$neo4j_vol_dir/plugins"
    cp "$PROJECT_ROOT/conf/neo4j_setup/conf/neo4j.conf" "$neo4j_vol_dir/conf"
    cp "$PROJECT_ROOT/conf/neo4j_setup/imports/"* "$neo4j_vol_dir/import"

    sudo chown -R 7474:7474 "$neo4j_vol_dir"
    sudo chmod -R 775 "$neo4j_vol_dir"
}

# Updating DNS entries for local service resolution
update_dns_entries() {
    echo "Updating /etc/hosts with local DNS entries for rabbitmq and neo4j..."
    echo '127.0.0.1 rabbitmq' | sudo tee -a /etc/hosts
    echo '127.0.0.1 neo4j' | sudo tee -a /etc/hosts

    echo "Setup complete. All services should now be configured and started."
}

main() {
    clear_shadows
    check_prereqs
    setup_trap
    get_project_root
    configure_environment
    create_env_config_file
    generate_rabbit_certs
    configure_neo4j
    update_dns_entries
}

main
