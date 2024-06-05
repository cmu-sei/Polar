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

# If you want to use the GitLab Agent:
POLAR_CONFIG_GITLAB=0

# Commentary: We intend to support multiple alternatives for the graph and the
# pub/sub broker infrastructure. At present, our defaults are Neo4J and
# RabbitMQ. It's a bit redundant to have them as configurable options
# currently, but it does make it more clear that these are not hard
# requirements.

# If you want to use Neo4J for your graph:
POLAR_CONFIG_NEO4J=0

# If you want to use RabbitMQ for your pub/sub broker:
POLAR_CONFIG_RABBITMQ=0

# ********** End global configuration options **********


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
    local required="git openssl sudo make"
    for cmd in $required; do
        if ! command -v "$cmd" &> /dev/null; then
            echo "Error: Required command '$cmd' is not installed." >&2
            exit 1
        fi
    done
}

## Check for non-root
check_root() {
    if [ "$EUID" -eq 0 ]; then
        echo "Warning: You are running this script as root. It's recommended to run as a non-root user."
        echo "Press any key to continue as root or Ctrl+C to cancel."
        read -n 1 -s -r -p ""
    fi
}

# Signal handling for cleanup
setup_trap() {
    trap exit_handler EXIT
    trap "exit 2" INT
}

exit_handler() {
    # if the script exits on code 0 is successful, else perform cleanup.
    local ecode = $?
    if [ $ecode -eq 0 ]; then echo "Setup complete."
    else if [ $ecode -eq 2 ] then echo "Setup cancelled."
    else 
        echo "Script interrupted."
        delete_env_config
        delete_vars
    fi
}

get_project_root() {
    # Determine the script's directory and set the default project root to one level up
    local script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

    PROJECT_ROOT="$(dirname "$script_dir")"

    export PROJECT_ROOT=$PROJECT_ROOT
    
    echo "Project root set to: $PROJECT_ROOT"
}

# Define configuration through user input
configure_env() {
    local config_file="$PROJECT_ROOT/conf/env_setup.sh"
    if [[ -f "$config_file" ]]; then
        echo "Environment config file exists. Overwrite? [yN] " OVERWRITE
        if [[ $OVERWRITE =~ ^[yY]$ ]]; then
            rm "$config_file"
        else
            exit 2
        fi
    fi

    echo "Configuring the environment template. Please provide the required values."

    # TODO: Add support for other graph engines. Prompt the user for which engine they intend to use.
    read -p "Enter graph endpoint name [neo4j]: " GRAPH_ENDPOINT_NAME
    GRAPH_ENDPOINT_NAME=${GRAPH_ENDPOINT_NAME:-"neo4j"}
    GRAPH_ENDPOINT="neo4j://${GRAPH_ENDPOINT_NAME}:7687"

    # TODO: Add support for other brokers. Prompt the user for which broker they intend to use.
    read -p "Enter pub/sub broker endpoint name [rabbitmq]: " BROKER_ENDPOINT_NAME
    BROKER_ENDPOINT_NAME=${BROKER_ENDPOINT_NAME:-"rabbitmq"}
    BROKER_ENDPOINT="amqps://${BROKER_ENDPOINT_NAME}:5671/%2f?auth_mechanism=external"

    read -p "Enter graph username [neo4j]: " GRAPH_USER
    GRAPH_USER=${GRAPH_USER:-"neo4j"}

    read -p "Enter graph password [Password1]: " GRAPH_PASSWORD
    GRAPH_PASSWORD=${GRAPH_PASSWORD:-"Password1"}

    read -p "Enter graph database name [neo4j]: " GRAPH_DB
    GRAPH_DB=${GRAPH_DB:-"neo4j"}

    read -p "Enter TLS client key path [$PROJECT_ROOT/var/ssl/client_rabbitmq.p12]: " TLS_CLIENT_KEY
    TLS_CLIENT_KEY=${TLS_CLIENT_KEY:-"$PROJECT_ROOT/var/ssl/client_rabbitmq.p12"}

    read -p "Enter TLS key password (leave blank if none): " TLS_KEY_PASSWORD

    read -p "Enter TLS CA certificate path [$PROJECT_ROOT/var/ssl/ca_certificate.pem]: " TLS_CA_CERT
    TLS_CA_CERT=${TLS_CA_CERT:-"$PROJECT_ROOT/var/ssl/ca_certificate.pem"}


}

# Configuration is written to a file and sourced in the execution shell
create_env_config() {
    local config_file="$PROJECT_ROOT/conf/env_setup.sh"

    # This script is only meant to be run on an initial configuration. If the
    # config file exists, the user needs to modify it directly. Alternatively,
    # they can delete the config file and then this script can run.

    if [[ ! -f "$config_file" ]]; then
        echo "Creating configuration file at $config_file"

        echo '# Generated Environment Configuration. If you edit this, do not re-run dev_stack.sh' >> "$config_file"
        echo '# script without backing up this file, first.' >> "$config_file"

        echo '# The service endpoint of the given neo4j instance.' >> "$config_file"
        echo '# For local development, this could be "neo4j://neo4j:7687"' >> "$config_file"
        COMMAND="export GRAPH_ENDPOINT=\"$GRAPH_ENDPOINT\""
        eval "$COMMAND"
        echo "$COMMAND" >> "$config_file"

        echo '# The service endpoint of the broker instance. (for rabbitmq, prefix with amqp://)' >> "$config_file"
        echo '# For the development container, the name could be "rabbitmq". The auth mechanism,' >> "$config_file"
        echo '# if specified, is according to your configuration and follows the docs for' >> "$config_file"
        echo '# your chosen broker.' >> "$config_file"
        echo '# For our default, reference: "amqps://rabbitmq:5671/%2f?auth_mechanism=external"' >> "$config_file"
        COMMAND="export BROKER_ENDPOINT=\"$BROKER_ENDPOINT\""
        eval "$COMMAND"
        echo "$COMMAND" >> "$config_file"

        echo '# For the development container, this should be "neo4j"' >> "$config_file"
        COMMAND="export GRAPH_USER=\"$GRAPH_USER\""
        eval "$COMMAND"
        echo "$COMMAND" >> "$config_file"

        echo '# For the development container, this will be whatever you set it to be when' >> "$config_file"
        echo '# you set up your graph.' >> "$config_file"
        COMMAND="export GRAPH_PASSWORD=\"$GRAPH_PASSWORD\""
        eval "$COMMAND"
        echo "$COMMAND" >> "$config_file"

        echo '# For the development container, this might be "neo4j"' >> "$config_file"
        COMMAND="export GRAPH_DB=\"$GRAPH_DB\""
        eval "$COMMAND"
        echo "$COMMAND" >> "$config_file"

        echo '# The absolute file path to the client .p12 file. This is used by the Rust' >> "$config_file"
        echo '# binaries to auth with the broker via TLS.' >> "$config_file"
        COMMAND="export TLS_CLIENT_KEY=\"$TLS_CLIENT_KEY\""
        eval "$COMMAND"
        echo "$COMMAND" >> "$config_file"

        echo '# If a password was set for the .p12 file, put it here.' >> "$config_file"
        COMMAND="export TLS_KEY_PASSWORD=\"$TLS_KEY_PASSWORD\""
        eval "$COMMAND"
        echo "$COMMAND" >> "$config_file"

        echo '# The absolute file path to the ca_certificates.pem file created by TLS_GEN.' >> "$config_file"
        echo '# Used by the Rust binaries to auth with RabbitMQ via TLS.' >> "$config_file"
        COMMAND="export TLS_CA_CERT=\"$TLS_CA_CERT\""
        eval "$COMMAND"
        echo "$COMMAND" >> "$config_file"

        chmod 600 "$config_file"
        source "$config_file"
    else
        echo "Hm. You shouldn't have been able to get here."
        echo "Environment config file exists. This script is only meant to be"
        echo "run once. Edit the config directly or delete it to run this script again."
        echo "WARNING: Execution has failed and will clean-up any artifacts"
        echo "potentially created during this run."
        exit -1
    fi
}

# Configuration file is removed if it exists, as in the case of premature exit
delete_env_config() {
    local config_file="$PROJECT_ROOT/conf/env_setup.sh"

    if [[ -f "$config_file" ]]; then
        echo "Removing environmental config file..."
        rm "$config_file"
    else
        echo "Environmental config file was not generated."
    fi
}

# Generate SSL certificates for Rabbit
# This & configure_neo4j can be executed in either order
generate_certs() {
    # Check for /var and generate if it does not exist
    local var_dir="$PROJECT_ROOT/var"
    if [[ ! -d "$var_dir" ]]; then
        mkdir -p $PROJECT_ROOT/var
    fi
    # Clone tls-gen, generate SSL files, and set up Neo4J volumes
    echo "Generating SSL certificates using tls-gen..."
    cd $PROJECT_ROOT/var
    git clone https://github.com/rabbitmq/tls-gen.git
    cd tls-gen/basic
    make CN=rabbitmq

    # Ensure the SSL directory exists and move the generated certificates there
    echo "Moving generated SSL files to the designated directory..."
    local ssl_dir="$PROJECT_ROOT/var/ssl"
    mkdir -p "$ssl_dir"
    cp result/* "$ssl_dir"
    cd "$ssl_dir"
    cp client_rabbitmq.p12 client_rabbitmq.p12.original

# Convert the client .p12 file using OpenSSL
    echo "Converting client p12 file to legacy format using OpenSSL..."
    openssl pkcs12 -legacy -export -inkey client_rabbitmq_key.pem -in client_rabbitmq_certificate.pem -out client_rabbitmq.p12 -passout pass:""

# Adjust permissions for security
    echo "Adjusting permissions for SSL files..."
    sudo chown $(whoami):0 *
    sudo chmod 400 *
    sudo chmod 777 -R "$ssl_dir"
}

# Configure Neo4J in /var directory
# This & generate_certs can be executed in either order
configure_neo4j() {
    # Provisioning Neo4J directories and setting permissions
    echo "Setting up Neo4J directories and copying configuration files..."

    # Check for /var and generate if it does not exist
    local var_dir="$PROJECT_ROOT/var"
    if [[ ! -d "$var_dir" ]]; then
        mkdir -p $PROJECT_ROOT/var
    fi

    local neo4j_vol_dir="$PROJECT_ROOT/var/neo4j_volumes"

    mkdir -p $neo4j_vol_dir/conf
    mkdir -p $neo4j_vol_dir/data
    mkdir -p $neo4j_vol_dir/import
    mkdir -p $neo4j_vol_dir/logs
    mkdir -p $neo4j_vol_dir/plugins

    cp -r "$PROJECT_ROOT/conf/neo4j_setup/plugins/"* "$neo4j_vol_dir/plugins"
    cp "$PROJECT_ROOT/conf/neo4j_setup/conf/neo4j.conf" "$neo4j_vol_dir/conf"
    cp "$PROJECT_ROOT/conf/neo4j_setup/imports/"* "$neo4j_vol_dir/import"

    sudo chown -R 7474:7474 "$neo4j_vol_dir"
    sudo chmod -R 775 "$neo4j_vol_dir"
}

# Delete /var directory, which contains certificates & configuration for Neo4J and Rabbit
delete_vars() {
    local var_dir="$PROJECT_ROOT/var"

    # Fully delete /var folder if it exists.
    if [[ -d "$var_dir" ]]; then
        echo "Removing /var directory..."
        sudo rm -rf "$var_dir"
    else
        echo "Certificates and config were not generated."
    fi
}

# Updating DNS entries for local service resolution
update_dns_entries() {
    local file="/etc/hosts"

    echo "Updating $file with local DNS entries for the broker and the graph..."

    entry="127.0.0.1 $BROKER_ENDPOINT_NAME"
    if [[ -z $(grep -Fx "$entry" "$file") ]]; then
        echo "$entry" | sudo tee -a "$file"
    else
        echo "The line '$entry' already exists in $file. Leaving it alone."
    fi

    entry="127.0.0.1 $GRAPH_ENDPOINT_NAME"
    if [[ -z $(grep -Fx "$entry" "$file") ]]; then
        echo "$entry" | sudo tee -a "$file"
    else
        echo "The line '$entry' already exists in $file. Leaving it alone."
    fi
}

# Removing broker & graph entries from local service resolution
# Note: this is not used in the trap handler, since the update method (above)
# handles when the entries already exist. may be useful for an uninstaller.
remove_dns_entries() {
    local file="/etc/hosts"

    echo "Removing DNS entries for the broker and the graph from $file..."

    entry="127.0.0.1 $BROKER_ENDPOINT_NAME"
    sed -i "/$entry/d" "$file"

    entry="127.0.0.1 $GRAPH_ENDPOINT_NAME"
    sed -i "/$entry/d" "$file"
}

main() {
    clear_shadows
    check_prereqs
    check_root
    setup_trap

    echo "WARNING: This script makes modifications to your system, including"
    echo "configuration files, environment variables, and certificates. Back up files"
    echo "before running this script if you don't want them modified or deleted."
    echo ""
    echo "Press any key to continue or Ctrl+C to cancel."

    # Wait for user input or Ctrl+C
    read -n 1 -s -r -p ""

    get_project_root
    configure_env
    create_env_config
    if [[ POLAR_CONFIG_GITLAB -eq 0 ]]; then
        source "$PROJECT_ROOT/src/agents/gitlab/scripts/stack_init.sh"
    fi
    generate_certs

    # TODO: If we add support for other graphs and brokers, this will need to
    # be run conditionally.
    configure_neo4j

    update_dns_entries
}

main
