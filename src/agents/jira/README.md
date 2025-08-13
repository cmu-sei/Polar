Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING
INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON
UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS
TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE
OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE
MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND
WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact
permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release
and unlimited distribution.  Please see Copyright notice for non-US
Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject
to its own license.

DM24-0470

# Jira Agent

Three important parts of the framework implementation include:
* Jira Resource Observer
    * Requires credentials for Jira in the form of a private token configured with read and write access for the API as well as credentials for authenticating with the given rabbitmq instance. The Jira Observer is instantiated as a set of cooperating binaries, each handling a specific type of Jira data.
* Jira Message Consumer
    * The message consumer requires credentials for the rabbitmq instance as well and credentials for signing into a given neo4j graph database to publish information to. The consumer transforms specific Jira data into domain-specified nodes and edges that can be used to tie CI/CD concepts to other domain knowledge.
* The Types Library
    * Contains implementation of the various Jira types, as well as implementations  for serialization / deserialization.

All credentials, endpoints, and the like should be read in as environment variables,possibly from an environment file. There is an example an environment file in the Jira agent [README](../../docs/README_Jira.md) in the manual setup.


# Development Setup

There are some first time components before the Jira agent can be ran. When doing local development, the rust binaries should be compiled and ran locally outside a container. More will be explained further down. 


# Manual Setup
[skip if you have already run the automated setup above or prefer ]

This tool requires the following values to be present in your environment as
variables - some of which may contain sensitive data and should be stored
securely. See your team about how to retrieve this information.
1. Create an environments file named `conf/env_setup.sh` with the following template.
```sh

# Generated Environment Configuration. If you edit this, do not re-run dev_stack.sh
# script without backing up this file, first.
# The service endpoint of the given neo4j instance.
# For local development, this could be "neo4j://neo4j:7687"
export GRAPH_ENDPOINT=""
# The service endpoint of the broker instance.
# For the development container, the name could be "cassini" or default to 127.0.0.1:PORT
export BROKER_ADDR=""
#The "identity" the clients will expect the broker to have
export CASSINI_SERVER_NAME=""
# For the development container, this should be "neo4j"
export GRAPH_USER=""
# For the development container, this will be whatever you set it to be when
# you set up your graph.
export GRAPH_PASSWORD=""
# For the development container, this might be "neo4j"
export GRAPH_DB=""
# The absolute file path to the client key .pem file. This is used by the Rust
# binaries to auth with the broker via TLS.
export TLS_CLIENT_KEY=""
# If a password was set for the client certificates file, put it here.
export TLS_KEY_PASSWORD=""
# The absolute file path to the ca_certificates.pem file created by TLS_GEN.
# Used by the Rust binaries to auth with RabbitMQ via TLS.
export TLS_CA_CERT=""
# The absolute file path to the observer_config.yaml. One is available in the
# following dir: ./Jira_agent/src/observer/src/observer_config.yaml
export JIRA_OBSERVER_CONFIG=""
# The REST base url of the Jira instance
export JIRA_URL=""
# A Personal Access Token for the instance (Note: The information returned from
# Jira will depend on the permissions granted to the token
# See Jiras REST API docs for more information
# For reference, Jira tokens use the form, "glpat-xxxxxxxxxxxxxxxxxxxx"
export JIRA_TOKEN=""
```
